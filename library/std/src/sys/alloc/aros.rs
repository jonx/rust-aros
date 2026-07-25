//! System allocator for AROS.
//!
//! A thin wrapper over `posixc` `malloc`/`calloc`/`realloc`/`free`/
//! `posix_memalign`, declared directly here so `std` needs no `libc`-crate AROS
//! support yet. Mirrors `sys/alloc/unix.rs` (the MIN_ALIGN fast path + a
//! `posix_memalign` slow path for over-aligned requests).
use super::{MIN_ALIGN, realloc_fallback};
use crate::alloc::{GlobalAlloc, Layout, System};
use crate::ptr;

mod c {
    unsafe extern "C" {
        pub fn malloc(size: usize) -> *mut u8;
        pub fn calloc(nmemb: usize, size: usize) -> *mut u8;
        pub fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
        pub fn free(ptr: *mut u8);
        pub fn posix_memalign(memptr: *mut *mut u8, align: usize, size: usize) -> i32;
    }
}

#[stable(feature = "alloc_system_type", since = "1.28.0")]
unsafe impl GlobalAlloc for System {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
            unsafe { c::malloc(layout.size()) }
        } else {
            unsafe { aligned_malloc(&layout) }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
            unsafe { c::calloc(layout.size(), 1) }
        } else {
            let ptr = unsafe { self.alloc(layout) };
            if !ptr.is_null() {
                unsafe { ptr::write_bytes(ptr, 0, layout.size()) };
            }
            ptr
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { c::free(ptr) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Only a block that actually came from `malloc` may be handed to
        // `realloc`. AROS's `aligned_alloc` (behind `posix_memalign`) returns a
        // pointer *into* a larger malloc block, with the real pointer and a
        // magic marker stored just before it; `free` knows to look for that,
        // `realloc` does not, and reads the block header at the wrong offset.
        //
        // So the test has to mirror `alloc`'s exactly -- on the ORIGINAL layout,
        // not on `new_size`. The unix version gates on `new_size`, which is fine
        // where aligned allocations share malloc's representation, but here it
        // sends every grown over-aligned block through the wrong path and
        // corrupts the heap.
        if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
            unsafe { c::realloc(ptr, new_size) }
        } else {
            unsafe { realloc_fallback(self, ptr, layout, new_size) }
        }
    }
}

#[inline]
unsafe fn aligned_malloc(layout: &Layout) -> *mut u8 {
    // posix_memalign requires the alignment be a multiple of sizeof(void*).
    let mut out: *mut u8 = ptr::null_mut();
    let align = layout.align().max(size_of::<usize>());
    let ret = unsafe { c::posix_memalign(&mut out, align, layout.size()) };
    if ret != 0 { ptr::null_mut() } else { out }
}
