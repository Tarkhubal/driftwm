use smithay::backend::renderer::{
    element::{
        AsRenderElements,
        surface::WaylandSurfaceRenderElement,
    },
    gles::GlesRenderer,
};
use smithay::desktop::layer_map_for_output;
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Physical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

use super::blur::{BlurLayer, BlurRequestData};
use super::elements::{OutputRenderElements, PixelSnapRescaleElement};

/// Build render elements for canvas-positioned layer surfaces (zoomed like windows).
/// Mirrors the window pipeline: position relative to camera, then RescaleRenderElement for zoom.
pub(super) fn build_canvas_layer_elements(
    state: &crate::state::DriftWm,
    renderer: &mut GlesRenderer,
    output: &Output,
    camera: Point<f64, Logical>,
    zoom: f64,
) -> Vec<OutputRenderElements> {
    let output_scale = output.current_scale().fractional_scale();
    let mut elements = Vec::new();

    for cl in &state.canvas_layers {
        let Some(pos) = cl.position else { continue; };
        // Camera-relative position (same as render_elements_for_region does for windows)
        let rel: Point<f64, Logical> = Point::from((
            pos.x as f64 - camera.x,
            pos.y as f64 - camera.y,
        ));
        let physical_loc = rel.to_physical_precise_round(output_scale);

        let surface_elements = cl
            .surface
            .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                renderer,
                physical_loc,
                smithay::utils::Scale::from(output_scale),
                1.0,
            );
        elements.extend(surface_elements.into_iter().map(|elem| {
            OutputRenderElements::Window(PixelSnapRescaleElement::from_element(
                elem,
                Point::<i32, Physical>::from((0, 0)),
                zoom,
            ))
        }));
    }

    elements
}

/// Build render elements for all layer surfaces on the given layer.
/// Layer surfaces are screen-fixed (not zoomed), so they use raw WaylandSurfaceRenderElement.
///
/// When `blur_config` is `Some`, layer surfaces whose `namespace()` matches a window rule
/// with `blur = true` will produce `BlurRequestData` entries alongside their render elements.
pub(super) fn build_layer_elements(
    output: &Output,
    renderer: &mut GlesRenderer,
    layer: WlrLayer,
    blur_config: Option<(&driftwm::config::Config, bool, BlurLayer)>,
) -> (Vec<OutputRenderElements>, Vec<BlurRequestData>) {
    let map = layer_map_for_output(output);
    let output_scale = output.current_scale().fractional_scale();
    let mut elements = Vec::new();
    let mut blur_requests = Vec::new();

    for surface in map.layers_on(layer).rev() {
        let geo = map.layer_geometry(surface).unwrap_or_default();
        let loc = geo.loc.to_physical_precise_round(output_scale);

        let elem_start = elements.len();
        elements.extend(
            surface
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    loc,
                    smithay::utils::Scale::from(output_scale),
                    1.0,
                )
                .into_iter()
                .map(OutputRenderElements::Layer),
        );

        if let Some((config, blur_enabled, layer_tag)) = blur_config
            && blur_enabled
        {
            let rule_blur = config
                .resolve_window_rules(surface.namespace(), "")
                .is_some_and(|r| r.blur);
            let client_blur_rects = with_states(surface.wl_surface(), |s| {
                crate::handlers::background_effect::get_cached_blur_region(s)
            });
            let client_blur = client_blur_rects.as_ref().is_some_and(|r| !r.is_empty());

            if rule_blur || client_blur {
                // Skip the request when the surface has no render elements yet
                // (e.g., layer surface mapped but client hasn't attached its first
                // buffer). Otherwise the mask pass renders zero elements into
                // bg_tex, leaving alpha=0, and the alpha-multiply blend zeros the
                // blur out — visible as missing blur on first frame.
                let elem_count = elements.len() - elem_start;
                if elem_count > 0 {
                    let screen_rect = geo.to_physical_precise_round(output_scale);

                    // Layer-shell: composite_scale = output_scale (no zoom — layers
                    // are screen-anchored). Surface origin = screen_rect.loc, so no
                    // additional offset within the mask.
                    let region_rects = if client_blur {
                        let rects = client_blur_rects.as_ref().unwrap();
                        let win_bounds: Rectangle<i32, Physical> =
                            Rectangle::from_size(screen_rect.size);
                        let mut out: Vec<Rectangle<i32, Physical>> =
                            Vec::with_capacity(rects.len());
                        for r in rects.iter() {
                            let x1 = (r.loc.x as f64 * output_scale).round() as i32;
                            let y1 = (r.loc.y as f64 * output_scale).round() as i32;
                            let x2 = ((r.loc.x + r.size.w) as f64 * output_scale).round() as i32;
                            let y2 = ((r.loc.y + r.size.h) as f64 * output_scale).round() as i32;
                            let phys: Rectangle<i32, Physical> =
                                Rectangle::from_extremities((x1, y1), (x2, y2));
                            if let Some(clipped) = phys.intersection(win_bounds) {
                                out.push(clipped);
                            }
                        }
                        if out.is_empty() { None } else { Some(std::sync::Arc::new(out)) }
                    } else {
                        None
                    };

                    // All client rects clipped out and no rule asked for blur →
                    // skip. region_rects=None would otherwise mean whole-window
                    // blur, contradicting the client's specific (off-window)
                    // request.
                    let skip_clipped_out = client_blur && region_rects.is_none() && !rule_blur;

                    if !skip_clipped_out {
                        blur_requests.push(BlurRequestData {
                            surface_id: surface.wl_surface().id(),
                            screen_rect,
                            elem_start,
                            elem_count,
                            layer: layer_tag,
                            region_rects,
                        });
                    }
                }
            }
        }
    }

    (elements, blur_requests)
}
