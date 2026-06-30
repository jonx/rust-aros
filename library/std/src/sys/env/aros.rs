//! env for AROS, over posixc `getenv`/`setenv`/`unsetenv`.
//!
//! AROS has no POSIX `environ` array (env vars are AROS-local / `ENV:`), so `env()`
//! (the full enumeration behind `std::env::vars`) returns empty for now;
//! `var`/`set_var`/`remove_var` work. OsStr on AROS is the raw-bytes representation,
//! so `as_encoded_bytes` is the exact byte round-trip.
pub use super::common::Env;
use crate::ffi::{CStr, CString, OsStr, OsString, c_char};
use crate::io;

mod c {
    use super::c_char;
    unsafe extern "C" {
        pub fn getenv(name: *const c_char) -> *mut c_char;
        pub fn setenv(name: *const c_char, value: *const c_char, overwrite: i32) -> i32;
        pub fn unsetenv(name: *const c_char) -> i32;
    }
}

fn cstr(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_encoded_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// Full enumeration is unsupported on AROS (no `environ`); returns empty.
pub fn env() -> Env {
    Env::new(Vec::new())
}

pub fn getenv(k: &OsStr) -> Option<OsString> {
    let key = cstr(k).ok()?;
    let v = unsafe { c::getenv(key.as_ptr()) };
    if v.is_null() {
        None
    } else {
        let bytes = unsafe { CStr::from_ptr(v) }.to_bytes().to_vec();
        Some(unsafe { OsString::from_encoded_bytes_unchecked(bytes) })
    }
}

pub unsafe fn setenv(k: &OsStr, v: &OsStr) -> io::Result<()> {
    let key = cstr(k)?;
    let val = cstr(v)?;
    if unsafe { c::setenv(key.as_ptr(), val.as_ptr(), 1) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub unsafe fn unsetenv(n: &OsStr) -> io::Result<()> {
    let key = cstr(n)?;
    if unsafe { c::unsetenv(key.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
