use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesRenderer, GlesTexture, Uniform, element::PixelShaderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use driftwm::config::BackgroundKind;

use super::elements::TileShaderElement;
use super::shaders::{BG_UNIFORMS, compile_tile_bg_shader, compile_wallpaper_bg_shader};

/// Update the cached background shader element for the current camera/zoom.
/// Returns (camera_moved, zoom_changed) for the caller's damage logic.
pub fn update_background_element(
    state: &mut crate::state::DriftWm,
    output: &Output,
    cur_camera: Point<f64, Logical>,
    cur_zoom: f64,
    last_rendered_camera: Point<f64, Logical>,
    last_rendered_zoom: f64,
) -> (bool, bool) {
    let camera_moved = cur_camera != last_rendered_camera;
    let zoom_changed = cur_zoom != last_rendered_zoom;
    let output_name = output.name();
    let output_size = crate::state::output_logical_size(output);
    let canvas_w = (output_size.w as f64 / cur_zoom).ceil() as i32;
    let canvas_h = (output_size.h as f64 / cur_zoom).ceil() as i32;
    let canvas_area = Rectangle::from_size((canvas_w, canvas_h).into());

    // Only push new uniforms when something the shader consumes has changed —
    // smithay's update_uniforms unconditionally bumps the element's CommitCounter,
    // which would otherwise cause the full-screen bg to damage every frame and
    // force re-composition of every element above it (blur especially).
    let animated = state.render.background_is_animated;
    let uniforms_stale = camera_moved || zoom_changed || animated;

    if let Some(elem) = state.render.cached_bg_elements.get_mut(&output_name) {
        elem.resize(canvas_area, Some(vec![canvas_area]));
        if uniforms_stale {
            let time_secs = state.start_time.elapsed().as_secs_f32();
            elem.update_uniforms(vec![
                Uniform::new("u_camera", (cur_camera.x as f32, cur_camera.y as f32)),
                Uniform::new("u_time", time_secs),
            ]);
        }
    } else if let Some(elem) = state.render.cached_tile_bg.get_mut(&output_name) {
        elem.resize(canvas_area, Some(vec![canvas_area]));
        if camera_moved || zoom_changed {
            elem.update_uniforms(vec![
                Uniform::new("u_camera", (cur_camera.x as f32, cur_camera.y as f32)),
                Uniform::new("u_tile_size", (elem.tex_w as f32, elem.tex_h as f32)),
                Uniform::new("u_output_size", (canvas_w as f32, canvas_h as f32)),
            ]);
        }
    } else if let Some(elem) = state.render.cached_wallpaper_bg.get_mut(&output_name) {
        // Viewport-fixed: size to the output (not the canvas), and never push uniforms.
        // Skipping update_uniforms keeps the CommitCounter stable across pans/zooms,
        // which is the whole point of wallpaper mode being cheaper than tile mode —
        // blur and elements above don't get damaged for background reasons.
        let output_area = Rectangle::from_size(output_size);
        elem.resize(output_area, Some(vec![output_area]));
    }
    (camera_moved, zoom_changed)
}

/// Compile background shader and/or load tile/wallpaper image.
/// Called at startup and on config reload (lazy re-init).
/// On failure, falls back to `DEFAULT_SHADER` — never leaves background uninitialized.
pub fn init_background(
    state: &mut crate::state::DriftWm,
    renderer: &mut GlesRenderer,
    initial_size: Size<i32, Logical>,
    output_name: &str,
) {
    // Each branch is responsible for leaving `background_is_animated` correct:
    //   - texture branches (tile/wallpaper) set it false on success.
    //   - shader branch re-derives it on cache miss; on cache hit (second
    //     output hotplug with the same shader) the flag is already correct
    //     from the first init.
    // Resetting eagerly here would clobber that flag on second-monitor cache
    // hits and silently freeze animated shaders on the new output.
    let texture_init: Option<bool> = match state.config.background.kind.clone() {
        BackgroundKind::Tile(path) => Some(try_init_texture_bg(
            state, renderer, initial_size, output_name, &path, TextureBgMode::Tile,
        )),
        BackgroundKind::Wallpaper(path) => Some(try_init_texture_bg(
            state, renderer, initial_size, output_name, &path, TextureBgMode::Wallpaper,
        )),
        BackgroundKind::Shader(_) | BackgroundKind::Default => None,
    };

    if texture_init != Some(true) {
        init_shader_bg(state, renderer, initial_size, output_name);
    }
}

#[derive(Copy, Clone)]
enum TextureBgMode {
    Tile,
    Wallpaper,
}

/// Returns true on success. On failure, the caller falls back to shader mode.
fn try_init_texture_bg(
    state: &mut crate::state::DriftWm,
    renderer: &mut GlesRenderer,
    initial_size: Size<i32, Logical>,
    output_name: &str,
    path: &str,
    mode: TextureBgMode,
) -> bool {
    let Some((texture, w, h)) = load_image_to_texture(renderer, path) else {
        return false;
    };

    let shader_slot = match mode {
        TextureBgMode::Tile => &mut state.render.tile_shader,
        TextureBgMode::Wallpaper => &mut state.render.wallpaper_shader,
    };
    if shader_slot.is_none() {
        *shader_slot = match mode {
            TextureBgMode::Tile => compile_tile_bg_shader(renderer),
            TextureBgMode::Wallpaper => compile_wallpaper_bg_shader(renderer),
        };
    }
    let Some(shader) = shader_slot.clone() else {
        tracing::error!("{:?} shader compilation failed, using default shader", match mode {
            TextureBgMode::Tile => "Tile",
            TextureBgMode::Wallpaper => "Wallpaper",
        });
        return false;
    };

    let area = Rectangle::from_size(initial_size);
    let uniforms = match mode {
        TextureBgMode::Tile => vec![
            Uniform::new("u_camera", (0.0f32, 0.0f32)),
            Uniform::new("u_tile_size", (w as f32, h as f32)),
            Uniform::new("u_output_size", (initial_size.w as f32, initial_size.h as f32)),
        ],
        // Wallpaper shader has no camera/zoom/time uniforms — image stretches to v_coords [0,1].
        TextureBgMode::Wallpaper => vec![],
    };
    let elem = TileShaderElement::new(
        shader,
        texture,
        w,
        h,
        area,
        Some(vec![area]),
        1.0,
        uniforms,
        Kind::Unspecified,
    );
    let target = match mode {
        TextureBgMode::Tile => &mut state.render.cached_tile_bg,
        TextureBgMode::Wallpaper => &mut state.render.cached_wallpaper_bg,
    };
    target.insert(output_name.to_string(), elem);
    // Tile/wallpaper modes have no per-frame uniform updates, so the
    // animated-flag must be false. (A stale `true` from a prior animated
    // shader would force every-frame redraws and defeat damage savings.)
    state.render.background_is_animated = false;
    true
}

fn load_image_to_texture(
    renderer: &mut GlesRenderer,
    path: &str,
) -> Option<(GlesTexture, i32, i32)> {
    use smithay::backend::renderer::ImportMem;
    use smithay::utils::Buffer;

    let img = match image::open(path) {
        Ok(img) => img.into_rgba8(),
        Err(e) => {
            tracing::error!("Failed to load image {path}: {e}, using default shader");
            return None;
        }
    };
    let (w, h) = img.dimensions();
    let raw = img.into_raw();
    match renderer.import_memory(
        &raw,
        Fourcc::Abgr8888,
        Size::<i32, Buffer>::from((w as i32, h as i32)),
        false,
    ) {
        Ok(texture) => Some((texture, w as i32, h as i32)),
        Err(e) => {
            tracing::error!("Failed to upload texture from {path}: {e}, using default shader");
            None
        }
    }
}

fn init_shader_bg(
    state: &mut crate::state::DriftWm,
    renderer: &mut GlesRenderer,
    initial_size: Size<i32, Logical>,
    output_name: &str,
) {
    // Reuse cached shader if already compiled (avoids redundant GPU work
    // when multiple outputs each need a background element).
    let shader = if let Some(ref cached) = state.render.background_shader {
        cached.clone()
    } else {
        let shader_source = match &state.config.background.kind {
            BackgroundKind::Shader(path) => match std::fs::read_to_string(path) {
                Ok(src) => src,
                Err(e) => {
                    tracing::error!("Failed to read shader {path}: {e}, using default");
                    driftwm::config::DEFAULT_SHADER.to_string()
                }
            },
            _ => driftwm::config::DEFAULT_SHADER.to_string(),
        };

        let compiled = match renderer.compile_custom_pixel_shader(&shader_source, BG_UNIFORMS) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to compile shader: {e}, using default");
                renderer
                    .compile_custom_pixel_shader(driftwm::config::DEFAULT_SHADER, BG_UNIFORMS)
                    .expect("Default shader must compile")
            }
        };

        state.render.background_is_animated = shader_source.contains("uniform float u_time");
        state.render.background_shader = Some(compiled.clone());
        compiled
    };

    let area = Rectangle::from_size(initial_size);
    let time_secs = state.start_time.elapsed().as_secs_f32();
    state.render.cached_bg_elements.insert(output_name.to_string(), PixelShaderElement::new(
        shader,
        area,
        Some(vec![area]),
        1.0,
        vec![
            Uniform::new("u_camera", (0.0f32, 0.0f32)),
            Uniform::new("u_time", time_secs),
        ],
        Kind::Unspecified,
    ));
}
