//! The Rustty desktop terminal application.

pub mod accessibility;
pub mod deck;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod deck_device;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod deck_render;
pub mod input;
pub mod platform;
pub mod presentation;
pub mod search;
#[cfg(target_os = "windows")]
pub mod software_surface;
pub mod workspace;
