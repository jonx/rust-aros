//! AROS pal `Mutex`: a `pthread_mutex_t` owned by C (`aros_sync_glue.c`), addressed
//! through a zeroed, pinned byte buffer. Mirrors `pal/unix/sync/mutex.rs` but calls
//! the `aros_mtx_*` glue instead of `libc::pthread_*` (AROS has no `libc` crate yet).
#![forbid(unsafe_op_in_unsafe_fn)]

use crate::cell::UnsafeCell;
use crate::ffi::c_void;
use crate::io::Error;
use crate::pin::Pin;

// pthread_mutex_t is 136 bytes on AROS aarch64; over-size for safety (init() asserts).
const MUTEX_WORDS: usize = 20; // 160 bytes, 8-aligned

unsafe extern "C" {
    fn aros_mtx_size() -> usize;
    fn aros_mtx_init(m: *mut c_void) -> i32;
    fn aros_mtx_lock(m: *mut c_void) -> i32;
    fn aros_mtx_trylock(m: *mut c_void) -> i32;
    fn aros_mtx_unlock(m: *mut c_void) -> i32;
    fn aros_mtx_destroy(m: *mut c_void) -> i32;
}

pub struct Mutex {
    inner: UnsafeCell<[u64; MUTEX_WORDS]>,
}

impl Mutex {
    pub fn new() -> Mutex {
        // A zeroed buffer is a valid PTHREAD_MUTEX_INITIALIZER; `init` sets it up.
        Mutex { inner: UnsafeCell::new([0; MUTEX_WORDS]) }
    }

    pub(super) fn raw(&self) -> *mut c_void {
        self.inner.get() as *mut c_void
    }

    /// # Safety
    /// May only be called once per instance of `Self`.
    pub unsafe fn init(self: Pin<&mut Self>) {
        assert!(
            unsafe { aros_mtx_size() } <= MUTEX_WORDS * 8,
            "pthread_mutex_t larger than the pal buffer"
        );
        let r = unsafe { aros_mtx_init(self.raw()) };
        assert_eq!(r, 0, "pthread_mutex_init failed");
    }

    /// # Safety
    /// * `init` must have been called.
    /// * Destroying a locked mutex is UB.
    pub unsafe fn lock(self: Pin<&Self>) {
        let r = unsafe { aros_mtx_lock(self.raw()) };
        if r != 0 {
            panic!("failed to lock mutex: {}", Error::from_raw_os_error(r));
        }
    }

    /// # Safety
    /// `init` must have been called.
    pub unsafe fn try_lock(self: Pin<&Self>) -> bool {
        unsafe { aros_mtx_trylock(self.raw()) == 0 }
    }

    /// # Safety
    /// The mutex must be locked by the current thread.
    pub unsafe fn unlock(self: Pin<&Self>) {
        let r = unsafe { aros_mtx_unlock(self.raw()) };
        debug_assert_eq!(r, 0);
    }
}

impl !Unpin for Mutex {}

unsafe impl Send for Mutex {}
unsafe impl Sync for Mutex {}

impl Drop for Mutex {
    fn drop(&mut self) {
        // A never-initialized mutex is a zeroed buffer, which `destroy` tolerates.
        let _ = unsafe { aros_mtx_destroy(self.raw()) };
    }
}
