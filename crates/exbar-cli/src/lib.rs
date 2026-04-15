//! Exbar — floating folder-shortcut toolbar for Windows 11 File Explorer.
//!
//! This library exposes the out-of-process runtime modules so the
//! binary (`src/bin/exbar.rs`) and unit tests can share them. The
//! binary entry point wires these modules together into the CLI.

pub mod config;
pub mod contextmenu;
pub mod dragdrop;
pub mod explorer;
pub mod log;
pub mod navigate;
pub mod picker;
pub mod shell_windows;
pub mod theme;
pub mod toolbar;
