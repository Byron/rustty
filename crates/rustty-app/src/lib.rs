//! The Rustty desktop terminal application.

pub mod accessibility;
pub mod deck;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod deck_device;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod deck_render;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod handoff;
pub mod input;
pub mod outliner;
pub mod outliner_ui;
pub mod platform;
pub mod presentation;
pub mod search;
#[cfg(target_os = "windows")]
pub mod software_surface;
pub mod workspace;

pub mod tmuxx_copy;
pub mod tmuxx_menu;
pub mod tmuxx_target;
pub mod tmuxx_workspace;
