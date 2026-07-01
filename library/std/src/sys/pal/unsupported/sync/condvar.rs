//! AROS pal `Condvar`: a `pthread_cond_t` owned by C (`aros_sync_glue.c`), addressed
//! through a zeroed, pinned byte buffer. Mirrors `pal/unix/sync/condvar.rs` but calls
//! the `aros_cond_*` glue. `PRECISE_TIMEOUT = false` so the layer-1 wrapper
//! double-checks timeouts with `Instant` (the hosted RTC isn't host-synced, so the
//! absolute deadline the timed-wait computes is only relatively accurate).
#![forbid(unsafe_op_in_unsafe_fn)]

use super::Mutex;
use crate::cell::UnsafeCell;
use crate::ffi::c_void;
use crate::pin::Pin;
use crate::time::Duration;

// pthread_cond_t is 152 bytes on AROS aarch64; over-size for safety.
const COND_WORDS: usize = 22; // 176 bytes, 8-aligned

unsafe extern "C" {
    fn aros_cond_size() -> usize;
    fn aros_cond_init(c: *mut c_void) -> i32;
    fn aros_cond_signal(c: *mut c_void) -> i32;
    fn aros_cond_broadcast(c: *mut c_void) -> i32;
    fn aros_cond_wait(c: *mut c_void, m: *mut c_void) -> i32;
    fn aros_cond_timedwait(c: *mut c_void, m: *mut c_void, secs: u32, nsecs: u32) -> i32;
    fn aros_cond_destroy(c: *mut c_void) -> i32;
}

pub struct Condvar {
    inner: UnsafeCell<[u64; COND_WORDS]>,
}

impl Condvar {
    pub const PRECISE_TIMEOUT: bool = false;

    pub fn new() -> Condvar {
        Condvar { inner: UnsafeCell::new([0; COND_WORDS]) }
    }

    fn raw(&self) -> *mut c_void {
        self.inner.get() as *mut c_void
    }

    /// # Safety
    /// May only be called once per instance of `Self`.
    pub unsafe fn init(self: Pin<&mut Self>) {
        assert!(
            unsafe { aros_cond_size() } <= COND_WORDS * 8,
            "pthread_cond_t larger than the pal buffer"
        );
        let r = unsafe { aros_cond_init(self.raw()) };
        assert_eq!(r, 0, "pthread_cond_init failed");
    }

    /// # Safety
    /// `init` must have been called.
    #[inline]
    pub unsafe fn notify_one(self: Pin<&Self>) {
        let r = unsafe { aros_cond_signal(self.raw()) };
        debug_assert_eq!(r, 0);
    }

    /// # Safety
    /// `init` must have been called.
    #[inline]
    pub unsafe fn notify_all(self: Pin<&Self>) {
        let r = unsafe { aros_cond_broadcast(self.raw()) };
        debug_assert_eq!(r, 0);
    }

    /// # Safety
    /// * `init` must have been called.
    /// * `mutex` must be locked by the current thread, and only ever paired with this
    ///   condvar.
    pub unsafe fn wait(self: Pin<&Self>, mutex: Pin<&Mutex>) {
        let r = unsafe { aros_cond_wait(self.raw(), mutex.raw()) };
        debug_assert_eq!(r, 0);
    }

    /// # Safety
    /// Same as `wait`. Returns `true` if notified, `false` on timeout.
    pub unsafe fn wait_timeout(&self, mutex: Pin<&Mutex>, dur: Duration) -> bool {
        let secs = dur.as_secs().min(u32::MAX as u64) as u32;
        let r = unsafe { aros_cond_timedwait(self.raw(), mutex.raw(), secs, dur.subsec_nanos()) };
        r == 0
    }
}

impl !Unpin for Condvar {}

unsafe impl Send for Condvar {}
unsafe impl Sync for Condvar {}

impl Drop for Condvar {
    fn drop(&mut self) {
        let _ = unsafe { aros_cond_destroy(self.raw()) };
    }
}
