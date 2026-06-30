# Changelog

## Unreleased

- Reworked the udev backend to hold its DRM devices in a map keyed by KMS node
  (`DriftWm::udev_devices`) instead of a single `udev_device` handle, and made
  the render loop iterate the devices. Only the primary GPU is populated today;
  this is the structural prerequisite for adding a secondary GPU's device on
  hotplug. No behaviour change for a single-GPU setup.
