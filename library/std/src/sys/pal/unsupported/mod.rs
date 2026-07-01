#![deny(unsafe_op_in_unsafe_fn)]

mod common;
pub use common::*;

// AROS reuses the `unsupported` pal but has real threads via pthread, so it needs
// `sys::pal::sync` for the shared pthread Mutex/Condvar/Parker wrappers.
#[cfg(target_os = "aros")]
pub mod sync;
