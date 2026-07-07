//! Real errno for AROS, over posixc.
//!
//! errno is `*__stdc_geterrnoptr()` (a pointer into the task's StdCBase); messages
//! come from posixc `strerror`. AROS uses **NetBSD errno numbering** (the errno.h
//! comment says so, and the bsdsocket bridge confirmed it: a refused connect gave
//! errno 61 = ECONNREFUSED), so the ErrorKind map below uses NetBSD values.
use crate::io::ErrorKind;

unsafe extern "C" {
    fn __stdc_geterrnoptr() -> *mut i32;
    fn strerror(errnum: i32) -> *const u8;
}

pub fn errno() -> i32 {
    unsafe { *__stdc_geterrnoptr() }
}

#[allow(dead_code)]
pub fn set_errno(e: i32) {
    unsafe { *__stdc_geterrnoptr() = e };
}

pub fn error_string(errno: i32) -> String {
    unsafe {
        let p = strerror(errno);
        if p.is_null() {
            return format!("unknown error (errno {errno})");
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(core::slice::from_raw_parts(p, len)).into_owned()
    }
}

pub fn is_interrupted(code: i32) -> bool {
    code == 4 // EINTR
}

pub fn decode_error_kind(code: i32) -> ErrorKind {
    use ErrorKind::*;
    match code {
        1 => PermissionDenied,    // EPERM
        2 => NotFound,            // ENOENT
        4 => Interrupted,         // EINTR
        13 => PermissionDenied,   // EACCES
        17 => AlreadyExists,      // EEXIST
        20 => NotADirectory,      // ENOTDIR
        21 => IsADirectory,       // EISDIR
        22 => InvalidInput,       // EINVAL
        28 => StorageFull,        // ENOSPC
        30 => ReadOnlyFilesystem, // EROFS
        32 => BrokenPipe,         // EPIPE
        35 => WouldBlock,         // EAGAIN / EWOULDBLOCK
        48 => AddrInUse,          // EADDRINUSE
        49 => AddrNotAvailable,   // EADDRNOTAVAIL
        53 => ConnectionAborted,  // ECONNABORTED
        54 => ConnectionReset,    // ECONNRESET
        57 => NotConnected,       // ENOTCONN
        60 => TimedOut,           // ETIMEDOUT
        61 => ConnectionRefused,  // ECONNREFUSED
        _ => Uncategorized,
    }
}
