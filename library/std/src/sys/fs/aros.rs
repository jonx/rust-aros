//! fs for AROS, over posixc `open`/`read`/`write`/`lseek`/`close` (+ `unlink`/
//! `mkdir`/`rename`). `File` is real; metadata, directory listing, permissions,
//! symlinks etc. are stubs for now (return `Unsupported`) -- read/write a file is
//! the demonstrable slice. Based on `sys/fs/unsupported.rs`.
//!
//! `off_t` is 64-bit on aarch64 (`__WORDSIZE==64`), `mode_t` is 16-bit, and AROS
//! `open` is variadic. The access-mode flags are AROS-specific and do **not** follow
//! NetBSD: `O_RDONLY=0x1`, `O_WRONLY=0x2`, `O_RDWR=0x3` (`O_ACCMODE=0x3`), from
//! `compiler/crt/posixc/include/fcntl.h`. `O_RDONLY` is **not** 0 here, so a
//! read-only open must set bit 0 or AROS `open` rejects the (zero) access mode with
//! EINVAL. The create/misc flags (`O_CREAT=0x40`, `O_TRUNC=0x200`, `O_APPEND=0x400`)
//! do match.
use crate::ffi::{CString, OsString};
use crate::fmt;
use crate::fs::TryLockError;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut, SeekFrom};
use crate::path::{Path, PathBuf};
pub use crate::sys::fs::common::Dir;
use crate::sys::time::SystemTime;
use crate::sys::unsupported;

mod c {
    use crate::ffi::{c_char, c_int};
    pub type mode_t = u16;
    pub type off_t = i64;
    unsafe extern "C" {
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn read(fd: c_int, buf: *mut u8, count: usize) -> isize;
        pub fn write(fd: c_int, buf: *const u8, count: usize) -> isize;
        pub fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t;
        pub fn close(fd: c_int) -> c_int;
        pub fn unlink(path: *const c_char) -> c_int;
        pub fn rename(old: *const c_char, new: *const c_char) -> c_int;
        pub fn mkdir(path: *const c_char, mode: mode_t) -> c_int;
        pub fn rmdir(path: *const c_char) -> c_int;
    }
    pub const O_RDONLY: c_int = 0x0001;
    pub const O_WRONLY: c_int = 0x0002;
    pub const O_RDWR: c_int = 0x0003;
    pub const O_CREAT: c_int = 0x0040;
    pub const O_EXCL: c_int = 0x0080;
    pub const O_TRUNC: c_int = 0x0200;
    pub const O_APPEND: c_int = 0x0400;
    pub const SEEK_SET: c_int = 0;
    pub const SEEK_CUR: c_int = 1;
    pub const SEEK_END: c_int = 2;

    // file-type bits of st_mode (octal, standard values; AROS posixc <sys/stat.h>)
    pub const S_IFMT: u32 = 0o170000;
    pub const S_IFREG: u32 = 0o100000;
    pub const S_IFDIR: u32 = 0o040000;
    pub const S_IFLNK: u32 = 0o120000;

    /// Mirror of `struct aros_fileattr` in `hosted/rust/aros_fs_glue.c`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Attr {
        pub size: u64,
        pub mode: u32,
        pub nlink: u32,
        pub ino: u64,
        pub mtime_sec: i64,
        pub mtime_nsec: i64,
        pub atime_sec: i64,
        pub atime_nsec: i64,
        pub ctime_sec: i64,
        pub ctime_nsec: i64,
    }

    unsafe extern "C" {
        pub fn aros_stat(path: *const c_char, out: *mut Attr) -> c_int;
        pub fn aros_lstat(path: *const c_char, out: *mut Attr) -> c_int;
        pub fn aros_fstat(fd: c_int, out: *mut Attr) -> c_int;
    }
}

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

pub struct File {
    fd: i32,
}

#[derive(Clone)]
pub struct FileAttr {
    size: u64,
    mode: u32,
    mtime: (i64, i64),
    atime: (i64, i64),
}
pub struct ReadDir(!);
pub struct DirEntry(!);

impl FileAttr {
    fn from_raw(a: &c::Attr) -> FileAttr {
        FileAttr {
            size: a.size,
            mode: a.mode,
            mtime: (a.mtime_sec, a.mtime_nsec),
            atime: (a.atime_sec, a.atime_nsec),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct FileTimes {}
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilePermissions {
    mode: u32,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileType {
    mode: u32,
}
#[derive(Debug)]
pub struct DirBuilder {}

impl FileAttr {
    pub fn size(&self) -> u64 { self.size }
    pub fn perm(&self) -> FilePermissions { FilePermissions { mode: self.mode } }
    pub fn file_type(&self) -> FileType { FileType { mode: self.mode } }
    pub fn modified(&self) -> io::Result<SystemTime> { SystemTime::new(self.mtime.0, self.mtime.1) }
    pub fn accessed(&self) -> io::Result<SystemTime> { SystemTime::new(self.atime.0, self.atime.1) }
    pub fn created(&self) -> io::Result<SystemTime> {
        Err(io::const_error!(io::ErrorKind::Unsupported, "birth time is not available on AROS"))
    }
}

impl FilePermissions {
    pub fn readonly(&self) -> bool { self.mode & 0o222 == 0 }
    pub fn set_readonly(&mut self, readonly: bool) {
        if readonly { self.mode &= !0o222; } else { self.mode |= 0o222; }
    }
}

impl FileTimes {
    pub fn set_accessed(&mut self, _t: SystemTime) {}
    pub fn set_modified(&mut self, _t: SystemTime) {}
}

impl FileType {
    pub fn is_dir(&self) -> bool { self.mode & c::S_IFMT == c::S_IFDIR }
    pub fn is_file(&self) -> bool { self.mode & c::S_IFMT == c::S_IFREG }
    pub fn is_symlink(&self) -> bool { self.mode & c::S_IFMT == c::S_IFLNK }
}

impl fmt::Debug for ReadDir {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0 }
}
impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<io::Result<DirEntry>> { self.0 }
}
impl DirEntry {
    pub fn path(&self) -> PathBuf { self.0 }
    pub fn file_name(&self) -> OsString { self.0 }
    pub fn metadata(&self) -> io::Result<FileAttr> { self.0 }
    pub fn file_type(&self) -> io::Result<FileType> { self.0 }
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
        }
    }
    pub fn read(&mut self, read: bool) { self.read = read; }
    pub fn write(&mut self, write: bool) { self.write = write; }
    pub fn append(&mut self, append: bool) { self.append = append; }
    pub fn truncate(&mut self, truncate: bool) { self.truncate = truncate; }
    pub fn create(&mut self, create: bool) { self.create = create; }
    pub fn create_new(&mut self, create_new: bool) { self.create_new = create_new; }

    fn flags(&self) -> i32 {
        let mut f = if self.read && self.write {
            c::O_RDWR
        } else if self.write || self.append {
            c::O_WRONLY
        } else {
            c::O_RDONLY
        };
        if self.append {
            f |= c::O_APPEND;
        }
        if self.truncate {
            f |= c::O_TRUNC;
        }
        if self.create {
            f |= c::O_CREAT;
        }
        if self.create_new {
            f |= c::O_CREAT | c::O_EXCL;
        }
        f
    }
}

impl File {
    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        let p = cstr(path)?;
        // AROS `open` is variadic and reads the `mode` va_arg unconditionally, so
        // always pass it (ignored unless O_CREAT).
        let fd = unsafe { c::open(p.as_ptr(), opts.flags(), 0o666 as crate::ffi::c_int) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(File { fd }) }
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { c::read(self.fd, buf.as_mut_ptr(), buf.len()) };
        if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
    }
    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        match bufs.iter_mut().find(|b| !b.is_empty()) {
            Some(b) => self.read(b),
            None => Ok(0),
        }
    }
    pub fn is_read_vectored(&self) -> bool { false }
    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let mut tmp = [0u8; 512];
        let cap = cursor.capacity().min(tmp.len());
        let n = self.read(&mut tmp[..cap])?;
        cursor.append(&tmp[..n]);
        Ok(())
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe { c::write(self.fd, buf.as_ptr(), buf.len()) };
        if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
    }
    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match bufs.iter().find(|b| !b.is_empty()) {
            Some(b) => self.write(b),
            None => Ok(0),
        }
    }
    pub fn is_write_vectored(&self) -> bool { false }

    pub fn flush(&self) -> io::Result<()> { Ok(()) }

    pub fn seek(&self, pos: SeekFrom) -> io::Result<u64> {
        let (whence, off) = match pos {
            SeekFrom::Start(n) => (c::SEEK_SET, n as i64),
            SeekFrom::Current(n) => (c::SEEK_CUR, n),
            SeekFrom::End(n) => (c::SEEK_END, n),
        };
        let r = unsafe { c::lseek(self.fd, off, whence) };
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r as u64) }
    }
    pub fn tell(&self) -> io::Result<u64> {
        let r = unsafe { c::lseek(self.fd, 0, c::SEEK_CUR) };
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r as u64) }
    }
    pub fn size(&self) -> Option<io::Result<u64>> { None }

    pub fn file_attr(&self) -> io::Result<FileAttr> {
        let mut a: c::Attr = unsafe { crate::mem::zeroed() };
        if unsafe { c::aros_fstat(self.fd, &mut a) } == 0 {
            Ok(FileAttr::from_raw(&a))
        } else {
            Err(io::Error::last_os_error())
        }
    }
    pub fn fsync(&self) -> io::Result<()> { Ok(()) }
    pub fn datasync(&self) -> io::Result<()> { Ok(()) }
    pub fn lock(&self) -> io::Result<()> { Ok(()) }
    pub fn lock_shared(&self) -> io::Result<()> { Ok(()) }
    pub fn try_lock(&self) -> Result<(), TryLockError> { Ok(()) }
    pub fn try_lock_shared(&self) -> Result<(), TryLockError> { Ok(()) }
    pub fn unlock(&self) -> io::Result<()> { Ok(()) }
    pub fn truncate(&self, _size: u64) -> io::Result<()> { unsupported() }
    pub fn duplicate(&self) -> io::Result<File> { unsupported() }
    pub fn set_permissions(&self, _perm: FilePermissions) -> io::Result<()> { unsupported() }
    pub fn set_times(&self, _times: FileTimes) -> io::Result<()> { unsupported() }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe { c::close(self.fd) };
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File").field("fd", &self.fd).finish()
    }
}

impl DirBuilder {
    pub fn new() -> DirBuilder { DirBuilder {} }
    pub fn mkdir(&self, p: &Path) -> io::Result<()> {
        let c = cstr(p)?;
        if unsafe { c::mkdir(c.as_ptr(), 0o777) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }
}

pub fn readdir(_p: &Path) -> io::Result<ReadDir> { unsupported() }

pub fn unlink(p: &Path) -> io::Result<()> {
    let c = cstr(p)?;
    if unsafe { c::unlink(c.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    let a = cstr(old)?;
    let b = cstr(new)?;
    if unsafe { c::rename(a.as_ptr(), b.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub fn set_perm(_p: &Path, _perm: FilePermissions) -> io::Result<()> { unsupported() }
pub fn set_times(_p: &Path, _times: FileTimes) -> io::Result<()> { unsupported() }
pub fn set_times_nofollow(_p: &Path, _times: FileTimes) -> io::Result<()> { unsupported() }

pub fn rmdir(p: &Path) -> io::Result<()> {
    let c = cstr(p)?;
    if unsafe { c::rmdir(c.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub fn remove_dir_all(_path: &Path) -> io::Result<()> { unsupported() }
pub fn exists(path: &Path) -> io::Result<bool> {
    match stat(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
pub fn readlink(_p: &Path) -> io::Result<PathBuf> { unsupported() }
pub fn symlink(_original: &Path, _link: &Path) -> io::Result<()> { unsupported() }
pub fn link(_src: &Path, _dst: &Path) -> io::Result<()> { unsupported() }
pub fn stat(p: &Path) -> io::Result<FileAttr> {
    let path = cstr(p)?;
    let mut a: c::Attr = unsafe { crate::mem::zeroed() };
    if unsafe { c::aros_stat(path.as_ptr(), &mut a) } == 0 {
        Ok(FileAttr::from_raw(&a))
    } else {
        Err(io::Error::last_os_error())
    }
}
pub fn lstat(p: &Path) -> io::Result<FileAttr> {
    let path = cstr(p)?;
    let mut a: c::Attr = unsafe { crate::mem::zeroed() };
    if unsafe { c::aros_lstat(path.as_ptr(), &mut a) } == 0 {
        Ok(FileAttr::from_raw(&a))
    } else {
        Err(io::Error::last_os_error())
    }
}
pub fn canonicalize(_p: &Path) -> io::Result<PathBuf> { unsupported() }
pub fn copy(_from: &Path, _to: &Path) -> io::Result<u64> { unsupported() }
