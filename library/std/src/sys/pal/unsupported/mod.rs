#![deny(unsafe_op_in_unsafe_fn)]

mod common;
pub use common::*;

// AROS reuses the `unsupported` pal but has real threads via pthread, so it needs
// `sys::pal::sync` for the shared pthread Mutex/Condvar/Parker wrappers.
// (Not on m68k-AROS: the run68k stub OS is single-task and has no pthread; its
// sync primitives are the no_threads/unsupported fallbacks.)
#[cfg(all(target_os = "aros", not(target_arch = "m68k")))]
pub mod sync;
