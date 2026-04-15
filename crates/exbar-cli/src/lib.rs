//! Exbar — floating folder-shortcut toolbar for Windows 11 File Explorer.
//!
//! This library exposes the out-of-process runtime modules so the
//! binary (`src/bin/exbar.rs`) and unit tests can share them. The
//! binary entry point wires these modules together into the CLI.

pub mod clipboard;
pub mod config;
pub mod contextmenu;
pub mod dragdrop;
pub mod drop_effect;
pub mod error;
pub mod explorer;
pub mod hit_test;
pub mod layout;
pub mod log;
pub mod picker;
pub mod pointer;
pub mod shell_windows;
pub mod theme;
pub mod toolbar;
