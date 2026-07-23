//! Owned and borrowed Unix-like file descriptors.
//!
//! This module is supported on Unix platforms and WASI, which both use a
//! similar file descriptor system for referencing OS resources.

#![stable(feature = "os_fd", since = "1.66.0")]
#![deny(unsafe_op_in_unsafe_fn)]

// `RawFd`, `AsRawFd`, etc.
mod raw;

// `OwnedFd`, `AsFd`, etc.
mod owned;

// Implementations for `AsRawFd` etc. for network types.
#[cfg(not(any(target_os = "trusty", target_os = "aros")))]
mod net;

// Implementation of stdio file descriptor constants.
mod stdio;

#[cfg(test)]
mod tests;

// Export the types and traits for the public API.
#[stable(feature = "os_fd", since = "1.66.0")]
pub use owned::*;
#[stable(feature = "os_fd", since = "1.66.0")]
pub use raw::*;
#[unstable(feature = "stdio_fd_consts", issue = "150836")]
pub use stdio::*;

/// Minimal `libc`-shaped shim for AROS (which has no `libc` crate target):
/// just the fd-module needs — the standard stream numbers and the posixc
/// `close`/`dup` entry points. Aliased as `libc` inside `raw`/`owned`.
#[cfg(target_os = "aros")]
pub(crate) mod aros_libc {
    use super::raw::RawFd;
    pub const STDIN_FILENO: RawFd = 0;
    pub const STDOUT_FILENO: RawFd = 1;
    pub const STDERR_FILENO: RawFd = 2;
    unsafe extern "C" {
        pub fn close(fd: RawFd) -> i32;
        pub fn dup(oldfd: RawFd) -> RawFd;
    }
}
