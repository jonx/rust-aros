//! Threads for AROS, over `pthread.library` (via the `aros_thr_*` C glue).
//!
//! `pthread_create` maps to an AROS process; the glue owns the opaque
//! `pthread_attr_t` so we can honour the requested stack size. TLS for spawned
//! threads is valid because the library registers each pthread in its thread table
//! (see `sys/thread_local/key/aros.rs`).
//!
//! `sleep` uses dos `Delay` in the glue (20ms granularity). Like the rest of the
//! hosted timer path, threading shares the OS-wide x18 exposure until the
//! `-ffixed-x18` rebuild (NOTES.md); Rust code itself is already x18-safe
//! (`+reserve-x18`).

use crate::ffi::{CStr, c_int, c_uint, c_void};
use crate::mem::ManuallyDrop;
use crate::num::NonZero;
use crate::thread::ThreadInit;
use crate::time::Duration;
use crate::{io, ptr};

pub const DEFAULT_MIN_STACK_SIZE: usize = 256 * 1024;

unsafe extern "C" {
    fn aros_thr_spawn(
        stacksize: usize,
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
        out_tid: *mut c_uint,
    ) -> c_int;
    fn aros_thr_join(tid: c_uint) -> c_int;
    fn aros_thr_detach(tid: c_uint) -> c_int;
    fn aros_thr_yield();
    fn aros_thr_sleep(secs: c_uint, nsecs: c_uint) -> c_int;
}

pub struct Thread {
    id: c_uint,
}

// The id is a plain integer handle; a thread is safe to move across threads.
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    // unsafe: see thread::Builder::spawn_unchecked for safety requirements
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread> {
        let data = Box::into_raw(init);
        let mut tid: c_uint = 0;
        let ret = unsafe { aros_thr_spawn(stack, thread_start, data as *mut c_void, &mut tid) };
        return if ret == 0 {
            Ok(Thread { id: tid })
        } else {
            // The thread failed to start, so `data` was not consumed; reclaim it.
            drop(unsafe { Box::from_raw(data) });
            Err(io::Error::from_raw_os_error(ret))
        };

        extern "C" fn thread_start(data: *mut c_void) -> *mut c_void {
            unsafe {
                // Recreate the box leaked above and run the thread body.
                let init = Box::from_raw(data as *mut ThreadInit);
                let rust_start = init.init();
                rust_start();
            }
            ptr::null_mut()
        }
    }

    pub fn join(self) {
        let id = ManuallyDrop::new(self).id;
        let ret = unsafe { aros_thr_join(id) };
        assert!(ret == 0, "failed to join thread: {}", io::Error::from_raw_os_error(ret));
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        // Detach so the library reclaims the thread once it finishes.
        let _ = unsafe { aros_thr_detach(self.id) };
    }
}

pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    // Hosted AROS schedules its tasks itself; report a single logical unit.
    Ok(NonZero::new(1).unwrap())
}

pub fn current_os_id() -> Option<u64> {
    None
}

pub fn yield_now() {
    unsafe { aros_thr_yield() };
}

pub fn set_name(_name: &CStr) {
    // AROS task names aren't wired to pthread here; leave the Rust-side name only.
}

pub fn sleep(dur: Duration) {
    // Split into u32-second chunks so long sleeps don't overflow the glue's args.
    let mut secs = dur.as_secs();
    let nsecs = dur.subsec_nanos();
    while secs > u32::MAX as u64 {
        unsafe { aros_thr_sleep(u32::MAX, 0) };
        secs -= u32::MAX as u64;
    }
    unsafe { aros_thr_sleep(secs as c_uint, nsecs as c_uint) };
}
