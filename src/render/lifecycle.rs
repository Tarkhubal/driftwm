use std::time::Duration;

use smithay::backend::renderer::element::RenderElementStates;
use smithay::desktop::layer_map_for_output;
use smithay::input::pointer::CursorImageStatus;
use smithay::output::Output;

use driftwm::canvas;

/// Sync foreign-toplevel protocol state with the current window list.
/// Call once per frame iteration (not per-output).
pub fn refresh_foreign_toplevels(state: &mut crate::state::DriftWm) {
    let keyboard = state.seat.get_keyboard().unwrap();
    let focused = keyboard.current_focus().map(|f| f.0);
    let outputs: Vec<Output> = state.space.outputs().cloned().collect();
    driftwm::protocols::foreign_toplevel::refresh::<crate::state::DriftWm>(
        &mut state.foreign_toplevel_state,
        &state.space,
        focused.as_ref(),
        &outputs,
    );
}

/// Per-surface throttling state for frame callbacks. Tracks the (output,
/// sequence) at which we last delivered a frame callback. A client that
/// commits a fresh frame within the same vsync cycle does not get another
/// callback — without this, a vsync-ignoring client (e.g. some Wine games)
/// can busy-loop the compositor: commit -> we render -> we send callback ->
/// client commits immediately -> we render again, ad infinitum.
struct SurfaceFrameThrottlingState {
    last_sent_at: std::cell::RefCell<Option<(Output, u32)>>,
}

impl Default for SurfaceFrameThrottlingState {
    fn default() -> Self {
        Self { last_sent_at: std::cell::RefCell::new(None) }
    }
}

fn frame_callback_filter<'a>(
    output: &'a Output,
    sequence: u32,
) -> impl FnMut(
    &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    &smithay::wayland::compositor::SurfaceData,
) -> Option<Output> + Copy + 'a {
    move |_surface, states| {
        let throttling = states
            .data_map
            .get_or_insert(SurfaceFrameThrottlingState::default);
        let mut last = throttling.last_sent_at.borrow_mut();
        if let Some((last_output, last_sequence)) = &*last
            && last_output == output
            && *last_sequence == sequence
        {
            return None;
        }
        *last = Some((output.clone(), sequence));
        Some(output.clone())
    }
}

/// Update each visible surface's primary-scanout-output to `output`. Smithay
/// uses this to decide where to deliver presentation feedback. Must be called
/// after `compositor.render_frame()` so we have render-element states.
pub fn update_primary_scanout_output(
    state: &crate::state::DriftWm,
    output: &Output,
    states: &RenderElementStates,
) {
    use smithay::desktop::utils::update_surface_primary_scanout_output;
    use smithay::wayland::compositor::with_surface_tree_downward;
    use smithay::wayland::compositor::TraversalAction;

    for window in state.space.elements() {
        window.with_surfaces(|surface, surface_data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                surface_data,
                None,
                states,
                smithay::backend::renderer::element::default_primary_scanout_output_compare,
            );
        });
    }

    let layer_map = layer_map_for_output(output);
    for layer_surface in layer_map.layers() {
        layer_surface.with_surfaces(|surface, surface_data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                surface_data,
                None,
                states,
                smithay::backend::renderer::element::default_primary_scanout_output_compare,
            );
        });
    }
    drop(layer_map);

    for cl in &state.canvas_layers {
        with_surface_tree_downward(
            cl.surface.wl_surface(),
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, surface_data, _| {
                update_surface_primary_scanout_output(
                    surface,
                    output,
                    surface_data,
                    None,
                    states,
                    smithay::backend::renderer::element::default_primary_scanout_output_compare,
                );
            },
            |_, _, _| true,
        );
    }

    if let Some(lock_surface) = state.lock_surfaces.get(output) {
        with_surface_tree_downward(
            lock_surface.wl_surface(),
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, surface_data, _| {
                update_surface_primary_scanout_output(
                    surface,
                    output,
                    surface_data,
                    None,
                    states,
                    smithay::backend::renderer::element::default_primary_scanout_output_compare,
                );
            },
            |_, _, _| true,
        );
    }
}

/// Collect presentation-feedback callbacks from all surfaces visible on `output`.
/// Hand the result to `compositor.queue_frame()` and let `frame_submitted()`
/// return it to be consumed by `presented()` on VBlank.
pub fn take_presentation_feedback(
    state: &crate::state::DriftWm,
    output: &Output,
    states: &RenderElementStates,
) -> smithay::desktop::utils::OutputPresentationFeedback {
    use smithay::desktop::utils::{
        OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
        surface_primary_scanout_output, take_presentation_feedback_surface_tree,
    };

    let mut feedback = OutputPresentationFeedback::new(output);

    for window in state.space.elements() {
        window.take_presentation_feedback(
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }

    let layer_map = layer_map_for_output(output);
    for layer_surface in layer_map.layers() {
        layer_surface.take_presentation_feedback(
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }
    drop(layer_map);

    for cl in &state.canvas_layers {
        take_presentation_feedback_surface_tree(
            cl.surface.wl_surface(),
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }

    if let Some(lock_surface) = state.lock_surfaces.get(output) {
        take_presentation_feedback_surface_tree(
            lock_surface.wl_surface(),
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }

    feedback
}

/// Post-render: frame callbacks, space cleanup.
pub fn post_render(state: &mut crate::state::DriftWm, output: &Output) {
    let time = state.start_time.elapsed();
    let sequence = crate::state::output_state(output).frame_callback_sequence;

    // Only send frame callbacks to visible windows — off-screen clients
    // naturally throttle to zero FPS without callbacks.
    let (camera, zoom) = {
        let os = crate::state::output_state(output);
        (os.camera, os.zoom)
    };
    let viewport_size = crate::state::output_logical_size(output);
    let visible_rect = canvas::visible_canvas_rect(
        camera.to_i32_round(),
        viewport_size,
        zoom,
    );

    for window in state.space.elements() {
        let Some(loc) = state.space.element_location(window) else { continue };
        let geom_loc = window.geometry().loc;
        let mut bbox = window.bbox();
        bbox.loc += loc - geom_loc;
        if !visible_rect.overlaps(bbox) { continue }

        window.send_frame(output, time, Some(Duration::ZERO), frame_callback_filter(output, sequence));
    }

    // Layer surface frame callbacks
    {
        let layer_map = layer_map_for_output(output);
        for layer_surface in layer_map.layers() {
            layer_surface.send_frame(output, time, Some(Duration::ZERO), frame_callback_filter(output, sequence));
        }
    }

    // Canvas-positioned layer surface frame callbacks
    for cl in &state.canvas_layers {
        cl.surface.send_frame(output, time, Some(Duration::ZERO), frame_callback_filter(output, sequence));
    }

    // Cursor surface frame callbacks (animated cursors need these to advance)
    if let CursorImageStatus::Surface(ref surface) = state.cursor.cursor_status {
        smithay::desktop::utils::send_frames_surface_tree(
            surface, output, time, Some(Duration::ZERO),
            frame_callback_filter(output, sequence),
        );
    }

    // Lock surface frame callback
    if let Some(lock_surface) = state.lock_surfaces.get(output) {
        smithay::desktop::utils::send_frames_surface_tree(
            lock_surface.wl_surface(),
            output,
            time,
            Some(Duration::ZERO),
            frame_callback_filter(output, sequence),
        );
    }

    // Cleanup
    state.space.refresh();
    state.popups.cleanup();
    layer_map_for_output(output).cleanup();

    state.refresh_idle_inhibit();
}
