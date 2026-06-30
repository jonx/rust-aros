//! Stdio for AROS.
//!
//! Writes fd 1/2 and reads fd 0 directly through `posixc` `write`/`read`,
//! declared here so `std` needs no `libc`-crate AROS support yet. On hosted AROS
//! these fds are the `dos` Output()/Input() the shell set up for the program.
use crate::io;

mod c {
    use crate::ffi::c_void;
    unsafe extern "C" {
        pub fn write(fd: i32, buf: *const c_void, count: usize) -> isize;
        pub fn read(fd: i32, buf: *mut c_void, count: usize) -> isize;
    }
}

const STDIN_FILENO: i32 = 0;
const STDOUT_FILENO: i32 = 1;
const STDERR_FILENO: i32 = 2;
const EBADF: i32 = 9;

fn write_fd(fd: i32, buf: &[u8]) -> io::Result<usize> {
    let ret = unsafe { c::write(fd, buf.as_ptr().cast(), buf.len()) };
    if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(ret as usize) }
}

pub struct Stdin;
pub struct Stdout;
pub struct Stderr;

impl Stdin {
    pub const fn new() -> Stdin {
        Stdin
    }
}

impl io::Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let ret = unsafe { c::read(STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
        if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(ret as usize) }
    }
}

impl Stdout {
    pub const fn new() -> Stdout {
        Stdout
    }
}

impl io::Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_fd(STDOUT_FILENO, buf)
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stderr {
    pub const fn new() -> Stderr {
        Stderr
    }
}

impl io::Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_fd(STDERR_FILENO, buf)
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn is_ebadf(err: &io::Error) -> bool {
    err.raw_os_error() == Some(EBADF)
}

pub const STDIN_BUF_SIZE: usize = crate::sys::io::DEFAULT_BUF_SIZE;

pub fn panic_output() -> Option<impl io::Write> {
    Some(Stderr::new())
}
