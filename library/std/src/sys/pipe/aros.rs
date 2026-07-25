//! Pipes for AROS, over the `PIPE:` handler.
//!
//! An endpoint is a dos filehandle (a `BPTR`), not a posixc fd: AROS keeps
//! files, sockets and pipes in separate descriptor spaces, and only the dos one
//! reaches the pipe handler's private actions (non-blocking mode and read
//! readiness). So these go through the `aros_pipe_*` glue rather than the fs
//! pal.
//!
//! `pipe()` (the standalone `io::pipe` API) is not offered: the handler names
//! its pipes, and an unnamed pair has no use here yet. Child stdio pipes are
//! created by `sys::process`, which owns the naming.
#![forbid(unsafe_op_in_unsafe_fn)]

use crate::ffi::c_void;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::fmt;

/// A dos `BPTR`. Zero is the null handle.
pub type PipeHandle = isize;

unsafe extern "C" {
    fn aros_pipe_read(fh: PipeHandle, buf: *mut c_void, len: usize) -> isize;
    fn aros_pipe_write(fh: PipeHandle, buf: *const c_void, len: usize) -> isize;
    fn aros_pipe_close(fh: PipeHandle);
    fn aros_pipe_set_nonblock(fh: PipeHandle, enable: i32) -> i32;
}

pub struct Pipe {
    fh: PipeHandle,
}

#[inline]
pub fn pipe() -> io::Result<(Pipe, Pipe)> {
    Err(io::Error::UNSUPPORTED_PLATFORM)
}

impl Pipe {
    /// Adopt a handle the process glue opened. The `Pipe` owns it from here.
    pub(crate) fn from_handle(fh: PipeHandle) -> Pipe {
        Pipe { fh }
    }

    pub(crate) fn handle(&self) -> PipeHandle {
        self.fh
    }

    /// Non-blocking reads return `WouldBlock` on an empty pipe instead of
    /// waiting, which is what a reactor needs.
    pub(crate) fn set_nonblocking(&self, enable: bool) -> io::Result<()> {
        let r = unsafe { aros_pipe_set_nonblock(self.fh, enable as i32) };
        if r < 0 {
            Err(io::const_error!(io::ErrorKind::Uncategorized, "cannot set pipe non-blocking"))
        } else {
            Ok(())
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        // The handler allows one reader and one writer per pipe, so an endpoint
        // cannot be duplicated.
        Err(io::const_error!(
            io::ErrorKind::Unsupported,
            "a pipe endpoint cannot be duplicated on AROS"
        ))
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { aros_pipe_read(self.fh, buf.as_mut_ptr().cast(), buf.len()) };
        match n {
            -2 => Err(io::const_error!(io::ErrorKind::WouldBlock, "pipe is empty")),
            n if n < 0 => Err(io::Error::last_os_error()),
            n => Ok(n as usize),
        }
    }

    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let n = unsafe {
            aros_pipe_read(self.fh, cursor.as_mut().as_mut_ptr().cast(), cursor.capacity())
        };
        match n {
            -2 => Err(io::const_error!(io::ErrorKind::WouldBlock, "pipe is empty")),
            n if n < 0 => Err(io::Error::last_os_error()),
            n => {
                // SAFETY: the glue wrote exactly `n` bytes into the cursor.
                unsafe { cursor.advance(n as usize) };
                Ok(())
            }
        }
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        match bufs.iter_mut().find(|b| !b.is_empty()) {
            Some(b) => self.read(b),
            None => Ok(0),
        }
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn read_to_end(&self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let start = buf.len();
        let mut chunk = [0u8; 4096];
        loop {
            match self.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(buf.len() - start)
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe { aros_pipe_write(self.fh, buf.as_ptr().cast(), buf.len()) };
        if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match bufs.iter().find(|b| !b.is_empty()) {
            Some(b) => self.write(b),
            None => Ok(0),
        }
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.fh != 0 {
            unsafe { aros_pipe_close(self.fh) };
        }
    }
}

impl fmt::Debug for Pipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipe").field("handle", &self.fh).finish()
    }
}
