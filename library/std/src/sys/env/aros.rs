//! env for AROS, over posixc `getenv`/`setenv`/`unsetenv`.
//!
//! AROS has no POSIX `environ` array: env vars are the process's local variables
//! (`pr_LocalVars`, `LV_VAR` nodes) that `setenv`/`getenv` write and read. `env()`
//! (the enumeration behind `std::env::vars`) walks that list via the `aros_env_enum`
//! glue, so it returns exactly the local vars this process can see -- including
//! whatever `set_var` wrote. Global `ENV:` file variables are out of scope (they are
//! not part of this process's local set). `var`/`set_var`/`remove_var` work. OsStr on
//! AROS is the raw-bytes representation, so `as_encoded_bytes` is the exact byte
//! round-trip.
pub use super::common::Env;
use crate::ffi::{CStr, CString, OsStr, OsString, c_char, c_void};
use crate::io;

mod c {
    use super::{c_char, c_void};
    pub type EnvCb = extern "C" fn(
        ctx: *mut c_void,
        name: *const u8,
        name_len: usize,
        val: *const u8,
        val_len: usize,
    );
    unsafe extern "C" {
        pub fn getenv(name: *const c_char) -> *mut c_char;
        pub fn setenv(name: *const c_char, value: *const c_char, overwrite: i32) -> i32;
        pub fn unsetenv(name: *const c_char) -> i32;
        // walks pr_LocalVars, calling `cb(ctx, ...)` once per LV_VAR (aros_env_glue.c)
        pub fn aros_env_enum(cb: EnvCb, ctx: *mut c_void);
    }
}

fn cstr(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_encoded_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// Enumerate the process's local environment variables (`std::env::vars`).
pub fn env() -> Env {
    extern "C" fn push(
        ctx: *mut c_void,
        name: *const u8,
        name_len: usize,
        val: *const u8,
        val_len: usize,
    ) {
        // SAFETY: `ctx` is the `&mut Vec` we pass below; the slices are the glue's
        // LocalVar name/value, valid for the duration of this call.
        let vars = unsafe { &mut *(ctx as *mut Vec<(OsString, OsString)>) };
        let name = unsafe { crate::slice::from_raw_parts(name, name_len) };
        let val = if val.is_null() { &[][..] } else { unsafe { crate::slice::from_raw_parts(val, val_len) } };
        let key = unsafe { OsString::from_encoded_bytes_unchecked(name.to_vec()) };
        let value = unsafe { OsString::from_encoded_bytes_unchecked(val.to_vec()) };
        vars.push((key, value));
    }

    let mut vars: Vec<(OsString, OsString)> = Vec::new();
    unsafe { c::aros_env_enum(push, (&raw mut vars).cast::<c_void>()) };
    Env::new(vars)
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
