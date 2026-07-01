//! Command-line args for AROS.
//!
//! A `C:` command reaches Rust through a C `main(argc, argv)` (the harness), not
//! Rust's `lang_start`, so there's no automatic capture. The harness stashes them
//! in two C globals and this reads them. OsStr on AROS is raw bytes, so argv round-
//! trips exactly.
use crate::ffi::{CStr, OsString, c_char, c_int};
use crate::fmt;
use crate::vec;

unsafe extern "C" {
    static aros_argc: c_int;
    static aros_argv: *const *const c_char;
}

pub struct Args {
    iter: vec::IntoIter<OsString>,
}

pub fn args() -> Args {
    let mut v: Vec<OsString> = Vec::new();
    unsafe {
        if !aros_argv.is_null() {
            let mut i = 0isize;
            while i < aros_argc as isize {
                let p = *aros_argv.offset(i);
                if p.is_null() {
                    break;
                }
                let bytes = CStr::from_ptr(p).to_bytes().to_vec();
                v.push(OsString::from_encoded_bytes_unchecked(bytes));
                i += 1;
            }
        }
    }
    Args { iter: v.into_iter() }
}

impl fmt::Debug for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter.as_slice()).finish()
    }
}

impl Iterator for Args {
    type Item = OsString;
    fn next(&mut self) -> Option<OsString> {
        self.iter.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl DoubleEndedIterator for Args {
    fn next_back(&mut self) -> Option<OsString> {
        self.iter.next_back()
    }
}

impl ExactSizeIterator for Args {
    fn len(&self) -> usize {
        self.iter.len()
    }
}
