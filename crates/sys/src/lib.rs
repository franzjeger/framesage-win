//! Win32 wrappers for framesage-win.
//!
//! Everything here is gated on `cfg(windows)`. On non-Windows hosts this crate
//! compiles to an empty shell so the workspace stays buildable for things like
//! tooling and CI dry-runs.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod inner;

#[cfg(windows)]
pub use inner::*;

#[cfg(not(windows))]
mod stub;

#[cfg(not(windows))]
pub use stub::*;
