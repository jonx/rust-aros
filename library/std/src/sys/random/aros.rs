//! Randomness source for AROS.
//!
//! FIXME(aros): `posixc` has no `getrandom`/`arc4random_buf`/`/dev/urandom` yet,
//! so this seeds a SplitMix64 from address-space layout (a stack address, which
//! varies per run under ASLR) plus a per-call counter. That is enough for
//! `HashMap` keys to differ between runs; it is **not** cryptographically secure.
//!
//! Upgrade path (hosted darwin-aarch64): call the host `arc4random_buf` through
//! `hostlib.resource` — `OpenResource("hostlib.resource")` → `HostLib_Open(
//! "libSystem.dylib")` → `HostLib_GetPointer("arc4random_buf")`, the same idiom
//! `arch/all-unix/battclock` uses for host time. That needs a small AROS-side glue
//! and a review of host-call thread-safety from arbitrary task context (this
//! `fill_bytes` can run on any task, unlike battclock's init), so it is deliberately
//! deferred rather than shipped unsafely. On a native (non-hosted) AROS, wire this to
//! whatever entropy device the port grows. Either way, this file is the only change.
use crate::sync::atomic::{AtomicU64, Ordering};

pub fn fill_bytes(bytes: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let stack_seed = &bytes as *const _ as usize as u64;
    let mut x = STATE
        .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        .wrapping_add(stack_seed);
    for chunk in bytes.chunks_mut(8) {
        // SplitMix64
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let src = z.to_ne_bytes();
        for (d, s) in chunk.iter_mut().zip(src.iter()) {
            *d = *s;
        }
    }
}
