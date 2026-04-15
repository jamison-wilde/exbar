//! Pure drop-effect determination for the toolbar's `IDropTarget`.
//!
//! The OLE drag-drop protocol exposes keyboard modifiers and a dropeffect
//! value as bitflag-typed Win32 data. This module trades them in and out at
//! the adapter boundary (`dragdrop.rs`) so the decision logic can be tested
//! without touching COM.
