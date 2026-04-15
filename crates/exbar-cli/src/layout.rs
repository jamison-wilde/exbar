//! Pure-data toolbar layout: given folder names, their measured text widths,
//! DPI, orientation, and the grip size, compute the positions of every
//! button.
//!
//! No Win32 dependencies. No string measurement (caller pre-measures).
//! Fully unit-testable and `proptest`-friendly.
