use std::borrow::Cow;

use smithay::backend::renderer::{
    element::{Element, Kind, utils::RescaleRenderElement},
    gles::{
        GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
        element::PixelShaderElement,
    },
};
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};

use super::elements::{OutputRenderElements, corner_round_rect};

/// Uniform declarations for background shaders.
/// Shaders receive u_camera and u_time.
/// Zoom is handled externally via RescaleRenderElement.
pub(super) const BG_UNIFORMS: &[UniformName<'static>] = &[
    UniformName {
        name: Cow::Borrowed("u_camera"),
        type_: UniformType::_2f,
    },
    UniformName {
        name: Cow::Borrowed("u_time"),
        type_: UniformType::_1f,
    },
];

/// Shadow shader source — soft box-shadow around SSD windows.
const SHADOW_SHADER_SRC: &str = include_str!("../shaders/shadow.glsl");

/// Uniform declarations for the shadow shader.
pub(super) const SHADOW_UNIFORMS: &[UniformName<'static>] = &[
    UniformName {
        name: Cow::Borrowed("u_window_rect"),
        type_: UniformType::_4f,
    },
    UniformName {
        name: Cow::Borrowed("u_radius"),
        type_: UniformType::_1f,
    },
    UniformName {
        name: Cow::Borrowed("u_color"),
        type_: UniformType::_4f,
    },
    UniformName {
        name: Cow::Borrowed("u_corner_radius"),
        type_: UniformType::_1f,
    },
];

/// Compile the shadow shader program. Called once at startup alongside the background shader.
pub fn compile_shadow_shader(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    match renderer.compile_custom_pixel_shader(SHADOW_SHADER_SRC, SHADOW_UNIFORMS) {
        Ok(shader) => Some(shader),
        Err(e) => {
            tracing::error!("Failed to compile shadow shader: {e}");
            None
        }
    }
}

/// Key that fully determines the precise shadow uniforms.
/// `[body_x0, body_y0, body_x1, body_y1, shadow_x, shadow_y, shadow_w, shadow_h]`
/// in post-zoom physical pixels. Comparing consecutive keys tells us whether the
/// shadow element needs its uniforms refreshed (avoiding spurious commit bumps
/// during fully static frames).
pub type ShadowPhysKey = [i32; 8];

/// Compute both the uniforms and the phys key for a shadow element.
///
/// * `body_pre_zoom` — the body's pre-zoom physical rect, computed via
///   `to_physical_precise_round(output_scale)` at the call site. For SSD
///   this includes the title-bar strip; for CSD it's the content rect.
/// * `shadow_area` — logical rect of the shadow PixelShaderElement (body ± padding).
/// * `output_scale` — the output's fractional scale.
/// * `zoom` — current viewport zoom.
/// * `shadow_radius` — Gaussian blur extent passed through unchanged.
/// * `corner_radius_phys` — corner radius in post-zoom physical pixels.
///
/// The body's post-zoom rect is obtained via `corner_round_rect` (same chain
/// as `PixelSnapRescaleElement`); the shadow's post-zoom rect via
/// `upscale(zoom).to_i32_round()` (same chain as `RescaleRenderElement`).
/// Both go through `to_physical_precise_round` for the output-scale step first,
/// so this stays correct at fractional HiDPI — not just fractional zoom.
fn shadow_uniforms_precise(
    body_pre_zoom: Rectangle<i32, Physical>,
    shadow_area: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
    zoom: f64,
    shadow_radius: f32,
    corner_radius_phys: f32,
) -> (Vec<Uniform<'static>>, ShadowPhysKey) {
    use driftwm::config::DecorationConfig;
    let sc = DecorationConfig::SHADOW_COLOR;
    let zoom_scale = Scale::from(zoom);

    // Body post-zoom: corner rounding (matches PixelSnapRescaleElement).
    let body_post = corner_round_rect(body_pre_zoom.to_f64(), zoom_scale);

    // Shadow post-zoom: independent loc/size rounding (matches RescaleRenderElement
    // wrapping PixelShaderElement whose inner geometry = shadow_area.to_physical_precise_round).
    let shadow_pre: Rectangle<i32, Physical> = shadow_area.to_physical_precise_round(output_scale);
    let shadow_post: Rectangle<i32, Physical> = shadow_pre.to_f64().upscale(zoom_scale).to_i32_round();

    // Linear map: shader-logical pixels → post-zoom physical pixels.
    let phys_w = shadow_post.size.w.max(1) as f64;
    let phys_h = shadow_post.size.h.max(1) as f64;
    let logical_w = shadow_area.size.w.max(1) as f64;
    let logical_h = shadow_area.size.h.max(1) as f64;
    let px = phys_w / logical_w;
    let py = phys_h / logical_h;

    // Hole rect in shader-logical space — after interpolation the boundary
    // rasterizes at exactly the body's physical pixel edges.
    let hole_x = (body_post.loc.x - shadow_post.loc.x) as f64 / px;
    let hole_y = (body_post.loc.y - shadow_post.loc.y) as f64 / py;
    let hole_w = body_post.size.w as f64 / px;
    let hole_h = body_post.size.h as f64 / py;

    // Corner radius: from post-zoom physical back into shader-logical.
    let corner_logical = corner_radius_phys as f64 / px;

    let uniforms = vec![
        Uniform::new("u_window_rect", (
            hole_x as f32, hole_y as f32,
            hole_w as f32, hole_h as f32,
        )),
        Uniform::new("u_radius", shadow_radius),
        Uniform::new("u_color", (
            sc[0] as f32 / 255.0, sc[1] as f32 / 255.0,
            sc[2] as f32 / 255.0, sc[3] as f32 / 255.0,
        )),
        Uniform::new("u_corner_radius", corner_logical as f32),
    ];

    let key: ShadowPhysKey = [
        body_post.loc.x, body_post.loc.y,
        body_post.loc.x + body_post.size.w, body_post.loc.y + body_post.size.h,
        shadow_post.loc.x, shadow_post.loc.y,
        shadow_post.size.w, shadow_post.size.h,
    ];

    (uniforms, key)
}

/// Build (or reuse) a cached shadow `PixelShaderElement` for a window body and
/// push it into `target` wrapped in a `RescaleRenderElement`.
///
/// `body_logical` is the rect that casts the shadow — the title-bar+content
/// strip for SSD windows, or the content rect for CSD. The shadow rect is
/// derived by inflating it by `SHADOW_RADIUS.ceil()` on every side.
///
/// Cache invalidation:
/// * post-zoom phys key change → uniforms refreshed (geometry / scale / zoom moved)
/// * opacity change → element reconstructed (alpha is fixed at construction time)
#[allow(clippy::too_many_arguments)]
pub(super) fn push_shadow_element(
    target: &mut Vec<OutputRenderElements>,
    cache: &mut std::collections::HashMap<
        smithay::reexports::wayland_server::backend::ObjectId,
        crate::state::ShadowCacheEntry,
    >,
    surface_id: smithay::reexports::wayland_server::backend::ObjectId,
    shader: &GlesPixelProgram,
    body_logical: Rectangle<f64, Logical>,
    corner_radius_logical: f32,
    opacity: f64,
    output_scale: Scale<f64>,
    zoom: f64,
) {
    use driftwm::config::DecorationConfig;
    let shadow_radius = DecorationConfig::SHADOW_RADIUS;
    let pad = shadow_radius.ceil() as i32;

    let body_x = body_logical.loc.x.round() as i32;
    let body_y = body_logical.loc.y.round() as i32;
    let body_w = body_logical.size.w.round() as i32;
    let body_h = body_logical.size.h.round() as i32;
    let shadow_area = Rectangle::new(
        Point::<i32, Logical>::from((body_x - pad, body_y - pad)),
        Size::<i32, Logical>::from((body_w + 2 * pad, body_h + 2 * pad)),
    );

    let body_pre_zoom: Rectangle<i32, Physical> =
        body_logical.to_physical_precise_round(output_scale);
    let corner_r_phys = corner_radius_logical * output_scale.x as f32 * zoom as f32;
    let (fresh_uniforms, fresh_key) = shadow_uniforms_precise(
        body_pre_zoom, shadow_area, output_scale, zoom, shadow_radius, corner_r_phys,
    );

    // Alpha is baked into the element at construction; rebuild on opacity change.
    if cache
        .get(&surface_id)
        .is_some_and(|(elem, _)| (elem.alpha() - opacity as f32).abs() > f32::EPSILON)
    {
        cache.remove(&surface_id);
    }

    let (elem, cached_key) = cache.entry(surface_id).or_insert_with(|| {
        let elem = PixelShaderElement::new(
            shader.clone(),
            shadow_area,
            None,
            opacity as f32,
            fresh_uniforms.clone(),
            Kind::Unspecified,
        );
        (elem, Some(fresh_key))
    });

    if *cached_key != Some(fresh_key) {
        *cached_key = Some(fresh_key);
        elem.update_uniforms(fresh_uniforms);
    }
    elem.resize(shadow_area, None);
    target.push(OutputRenderElements::Background(
        RescaleRenderElement::from_element(
            elem.clone(),
            Point::<i32, Physical>::from((0, 0)),
            zoom,
        ),
    ));
}

const CORNER_CLIP_SRC: &str = include_str!("../shaders/corner_clip.glsl");

pub(super) const CORNER_CLIP_UNIFORMS: &[UniformName<'static>] = &[
    UniformName { name: Cow::Borrowed("aa_scale"), type_: UniformType::_1f },
    UniformName { name: Cow::Borrowed("geo_size"), type_: UniformType::_2f },
    UniformName { name: Cow::Borrowed("corner_radius"), type_: UniformType::_4f },
    UniformName { name: Cow::Borrowed("input_to_geo"), type_: UniformType::Matrix3x3 },
];

pub fn compile_corner_clip_shader(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    match renderer.compile_custom_texture_shader(CORNER_CLIP_SRC, CORNER_CLIP_UNIFORMS) {
        Ok(shader) => Some(shader),
        Err(e) => {
            tracing::error!("Failed to compile corner clip shader: {e}");
            None
        }
    }
}

const TILE_BG_SRC: &str = include_str!("../shaders/tile_bg.glsl");

pub(super) const TILE_BG_UNIFORMS: &[UniformName<'static>] = &[
    UniformName { name: Cow::Borrowed("u_camera"), type_: UniformType::_2f },
    UniformName { name: Cow::Borrowed("u_tile_size"), type_: UniformType::_2f },
    UniformName { name: Cow::Borrowed("u_output_size"), type_: UniformType::_2f },
];

pub(super) fn compile_tile_bg_shader(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    match renderer.compile_custom_texture_shader(TILE_BG_SRC, TILE_BG_UNIFORMS) {
        Ok(shader) => Some(shader),
        Err(e) => {
            tracing::error!("Failed to compile tile background shader: {e}");
            None
        }
    }
}

const WALLPAPER_BG_SRC: &str = include_str!("../shaders/wallpaper_bg.glsl");

pub(super) fn compile_wallpaper_bg_shader(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    match renderer.compile_custom_texture_shader(WALLPAPER_BG_SRC, &[]) {
        Ok(shader) => Some(shader),
        Err(e) => {
            tracing::error!("Failed to compile wallpaper background shader: {e}");
            None
        }
    }
}
