# Changelog

## Unreleased

- Made the capture path (wlr-screencopy and ext-image-copy-capture for outputs
  and toplevels) generic over the renderer: elements render through `R`, while
  pixel readback (`copy_framebuffer` / `map_texture`) runs on the primary GPU via
  `as_gles_renderer()`. The off-screen canvas screenshot path stays on the
  concrete `GlesRenderer`. No behaviour change on the single-GPU / winit path.
