//! Randomness source for AROS.
//!
//! Calls `posixc`'s `arc4random_buf`. On a hosted build that borrows the host libc's
//! real CSPRNG (via `hostlib.resource` → the host `arc4random_buf`); on a native AROS
//! it falls back to a weak, non-cryptographic mix. Either way the entropy policy lives
//! in AROS (`compiler/crt/posixc/arc4random.c`), not here, so this pal is just the
//! thin `extern "C"` call — and any other AROS code gets the same source.
use crate::ffi::c_void;

unsafe extern "C" {
    fn arc4random_buf(buf: *mut c_void, nbytes: usize);
}

pub fn fill_bytes(bytes: &mut [u8]) {
    if bytes.is_empty() {
        return;
    }
    unsafe { arc4random_buf(bytes.as_mut_ptr().cast::<c_void>(), bytes.len()) };
}
