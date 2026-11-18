//! Native desktop shell integrations.

#[cfg(windows)]
pub(crate) mod tray;

#[cfg(target_os = "macos")]
#[path = "tray_macos.rs"]
pub(crate) mod tray;
