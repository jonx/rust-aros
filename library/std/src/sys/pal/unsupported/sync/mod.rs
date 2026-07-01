//! AROS pal sync primitives (pthread mutex/cond via `aros_sync_glue.c`), exposed as
//! `sys::pal::sync` so the shared `pthread` `Mutex`/`Condvar`/`Parker` wrappers work.
#![forbid(unsafe_op_in_unsafe_fn)]

mod condvar;
mod mutex;

pub use condvar::Condvar;
pub use mutex::Mutex;
