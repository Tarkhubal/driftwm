# Changelog

## Unreleased

- Introduced a multi-GPU renderer manager on the udev backend: `Backend::Udev`
  now holds a `GpuManager` (one GLES renderer per DRM render node) plus the
  primary render node, and the render loop drives the primary output through
  `gpu_manager.single_renderer(node)` (a `MultiRenderer`). One-off renderer work
  (shader compilation, dmabuf import, off-screen screenshot) goes through a new
  `Backend::with_renderer` accessor. No behaviour change for a single-GPU setup;
  this is the groundwork for scanning out to displays on a secondary GPU.
