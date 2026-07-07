//! TLS keys for AROS, backed by `pthread.library` (the `-lpthread` linklib).
//!
//! Same shape as the `unix` key backend, but AROS has no `libc` crate binding yet,
//! so the four `pthread_key_*` functions are declared here directly. `pthread_key_t`
//! is `unsigned int` and the library reserves a slot for the main task at init
//! (`ADD2INIT`), so keys work on the main thread too, not just pthread-spawned ones.

use crate::ffi::{c_int, c_uint, c_void};
use crate::mem;

pub type Key = c_uint;

unsafe extern "C" {
    // Declared as Option<fn> (like the libc-crate binding) so that `None` is a VALID
    // value of the parameter type: transmuting None into a bare `extern "C" fn` would
    // materialize a null value of a non-nullable type, which is language-level UB
    // even before the call. Option<extern fn> is guaranteed null-pointer-optimized.
    fn pthread_key_create(
        key: *mut Key,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn pthread_getspecific(key: Key) -> *mut c_void;
    fn pthread_setspecific(key: Key, value: *const c_void) -> c_int;
    fn pthread_key_delete(key: Key) -> c_int;
}

#[inline]
pub fn create(dtor: Option<unsafe extern "C" fn(*mut u8)>) -> Key {
    let mut key = 0;
    // SAFETY: `dtor` has the same ABI as pthread's `void (*)(void *)`; None means
    // "no destructor" and stays None through the transmute of the Option.
    if unsafe { pthread_key_create(&mut key, mem::transmute(dtor)) } != 0 {
        rtabort!("out of TLS keys");
    }
    key
}

#[inline]
pub unsafe fn set(key: Key, value: *mut u8) {
    let r = unsafe { pthread_setspecific(key, value as *const c_void) };
    debug_assert_eq!(r, 0);
}

#[inline]
#[cfg(any(not(target_thread_local), test))]
pub unsafe fn get(key: Key) -> *mut u8 {
    unsafe { pthread_getspecific(key) as *mut u8 }
}

#[inline]
pub unsafe fn destroy(key: Key) {
    let r = unsafe { pthread_key_delete(key) };
    debug_assert_eq!(r, 0);
}
