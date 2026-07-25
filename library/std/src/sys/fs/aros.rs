//! fs for AROS, over posixc `open`/`read`/`write`/`lseek`/`close` (+ `unlink`/
//! `mkdir`/`rename`/`chmod`/`symlink`/`readlink`/`utimes`) plus the `aros_*` stat/
//! dir glue. `File`, metadata, directory listing, permissions (`chmod`/`fchmod`),
//! symlinks (`symlink`/`readlink`), and path `set_times` (`utimes`) are real, as
//! are `canonicalize` (posixc `realpath`), `File::truncate` (`ftruncate`),
//! `File::duplicate` (`dup`), and `copy`/`remove_dir_all` (the portable
//! `sys/fs/common.rs` impls). Still `Unsupported`: the fd-based `File::set_times`
//! and the nofollow `set_times` variant (posixc has no `futimes`/`lutimes`) and
//! `link` (posixc's `link()` is itself a stub that sets EPERM). Based on
//! `sys/fs/unsupported.rs`.
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
    use crate::ffi::{c_char, c_int, c_void};
    pub type mode_t = u16;
    pub type off_t = i64;
    unsafe extern "C" {
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn read(fd: c_int, buf: *mut u8, count: usize) -> isize;
        pub fn write(fd: c_int, buf: *const u8, count: usize) -> isize;
        pub fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t;
        pub fn close(fd: c_int) -> c_int;
        pub fn unlink(path: *const c_char) -> c_int;
        pub fn dup(fd: c_int) -> c_int;
        pub fn ftruncate(fd: c_int, length: off_t) -> c_int;
        // aros_fs_glue.c: Lock() + NameFromLock(). posixc realpath() is
        // unusable here -- it open(".")s to save the cwd, and "." is an
        // invalid component name to DOS, so it fails EINVAL on every input.
        pub fn aros_realpath(path: *const c_char, buf: *mut c_char, buflen: usize) -> c_int;
        pub fn rename(old: *const c_char, new: *const c_char) -> c_int;
        pub fn mkdir(path: *const c_char, mode: mode_t) -> c_int;
        pub fn rmdir(path: *const c_char) -> c_int;
        // directory listing (aros_fs_glue.c, over posixc opendir/readdir/closedir)
        pub fn aros_opendir(path: *const c_char) -> *mut c_void;
        pub fn aros_readdir(dir: *mut c_void, namebuf: *mut c_char, buflen: usize, type_out: *mut u32) -> c_int;
        pub fn aros_closedir(dir: *mut c_void);
    }
    // <aros/posixc/limits.h>: PATH_MAX = _XOPEN_PATH_MAX = 1024
    pub const PATH_MAX: usize = 1024;
    // d_type values from AROS posixc <dirent.h>
    pub const DT_DIR: u32 = 4;
    pub const DT_REG: u32 = 8;
    pub const DT_LNK: u32 = 10;
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
        // permissions + symlinks come straight from posixc (weak wrappers in
        // libposixc.a: chmod->SetProtection, symlink->MakeLink, readlink->ReadLink).
        pub fn chmod(path: *const c_char, mode: mode_t) -> c_int;
        pub fn fchmod(fd: c_int, mode: mode_t) -> c_int;
        pub fn symlink(target: *const c_char, linkpath: *const c_char) -> c_int;
        pub fn readlink(path: *const c_char, buf: *mut c_char, bufsiz: usize) -> isize;
        // set_times: aros_fs_glue.c builds the timeval[2] (posixc has utimes only).
        pub fn aros_utimes(
            path: *const c_char,
            atime_sec: i64,
            atime_nsec: i64,
            mtime_sec: i64,
            mtime_nsec: i64,
        ) -> c_int;
    }
}

/// Convert a path for the posixc syscall boundary, translating the
/// unix-join artifacts Rust callers produce into AROS device syntax:
///
/// - `SYS:/C` (what `PathBuf::join("SYS:", "C")` yields) -> `SYS:C` —
///   on AROS a slash right after the device colon means the device
///   root's *parent*, which no unix-minded caller ever intends.
/// - empty components (from `//` runs and trailing `/`) and `.`
///   components vanish — they are unix-join noise; on AROS `//` means
///   grandparent and `SYS:C/` locks the *parent* of C.
/// - `..` becomes an AROS parent step: an empty component, so
///   `a/../b` -> `a//b` ("b in a's parent") and a leading `../x`
///   -> `/x`. A path of only `..`s gets its final extra `/`
///   (`..` -> `/`, `../..` -> `//`).
///
/// A unix-style leading `/` on a colon-less path is preserved
/// unchanged (it already means "up" to AROS, matching what relative
/// unix paths that escape their base intend).
fn cstr(p: &Path) -> io::Result<CString> {
    let bytes = p.as_os_str().as_encoded_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    // Device prefix through the first ':' passes untouched; exactly one
    // '/' straight after the colon is a join artifact and dropped.
    let rest = match bytes.iter().position(|&b| b == b':') {
        Some(i) => {
            out.extend_from_slice(&bytes[..=i]);
            let mut r = &bytes[i + 1..];
            if r.first() == Some(&b'/') {
                r = &r[1..];
            }
            r
        }
        None => bytes,
    };
    let lead = out.is_empty() && rest.first() == Some(&b'/');
    let comps: Vec<&[u8]> = rest
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty() && *c != b".")
        .map(|c| if c == b".." { &b""[..] } else { c })
        .collect();
    if lead {
        out.push(b'/');
    }
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(c);
    }
    // k parent steps and nothing else: the join above emitted k-1
    // slashes; a parent step IS a slash on AROS, so add the missing one.
    if !comps.is_empty() && comps.iter().all(|c| c.is_empty()) {
        out.push(b'/');
    }
    CString::new(out).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
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
pub struct ReadDir {
    dir: *mut crate::ffi::c_void,
    root: PathBuf,
}
// The DIR* is owned solely by this ReadDir; safe to move/observe across threads.
unsafe impl Send for ReadDir {}
unsafe impl Sync for ReadDir {}

pub struct DirEntry {
    name: OsString,
    dtype: u32,
    root: PathBuf,
}

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
pub struct FileTimes {
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
}
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
    pub fn set_accessed(&mut self, t: SystemTime) { self.accessed = Some(t); }
    pub fn set_modified(&mut self, t: SystemTime) { self.modified = Some(t); }
}

impl FileType {
    pub fn is_dir(&self) -> bool { self.mode & c::S_IFMT == c::S_IFDIR }
    pub fn is_file(&self) -> bool { self.mode & c::S_IFMT == c::S_IFREG }
    pub fn is_symlink(&self) -> bool { self.mode & c::S_IFMT == c::S_IFLNK }
}

impl fmt::Debug for ReadDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadDir").field("root", &self.root).finish()
    }
}
impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<io::Result<DirEntry>> {
        let mut buf = [0u8; 1024];
        let mut dtype: u32 = 0;
        let r = unsafe {
            c::aros_readdir(self.dir, buf.as_mut_ptr() as *mut crate::ffi::c_char, buf.len(), &mut dtype)
        };
        match r {
            1 => {
                let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                let name = unsafe { OsString::from_encoded_bytes_unchecked(buf[..len].to_vec()) };
                Some(Ok(DirEntry { name, dtype, root: self.root.clone() }))
            }
            0 => None,
            _ => Some(Err(io::Error::last_os_error())),
        }
    }
}
impl Drop for ReadDir {
    fn drop(&mut self) {
        unsafe { c::aros_closedir(self.dir) };
    }
}
impl DirEntry {
    pub fn path(&self) -> PathBuf { self.root.join(&self.name) }
    pub fn file_name(&self) -> OsString { self.name.clone() }
    pub fn metadata(&self) -> io::Result<FileAttr> { stat(&self.path()) }
    pub fn file_type(&self) -> io::Result<FileType> {
        let mode = match self.dtype {
            c::DT_DIR => c::S_IFDIR,
            c::DT_REG => c::S_IFREG,
            c::DT_LNK => c::S_IFLNK,
            // DT_UNKNOWN or anything else: fall back to a stat
            _ => return self.metadata().map(|a| a.file_type()),
        };
        Ok(FileType { mode })
    }
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

    // Same access/creation-mode rules as std's unix pal: append implies write
    // (read+append must be O_RDWR, not O_WRONLY), create/create_new/truncate
    // require write or append, and truncate+append is invalid. EINVAL is 22 in
    // AROS's NetBSD errno numbering (see ../io/error/aros.rs).
    fn flags(&self) -> io::Result<i32> {
        const EINVAL: i32 = 22;
        let access = match (self.read, self.write, self.append) {
            (true, false, false) => c::O_RDONLY,
            (false, true, false) => c::O_WRONLY,
            (true, true, false) => c::O_RDWR,
            (false, _, true) => c::O_WRONLY | c::O_APPEND,
            (true, _, true) => c::O_RDWR | c::O_APPEND,
            (false, false, false) => return Err(io::Error::from_raw_os_error(EINVAL)),
        };
        let creation = match (self.write, self.append) {
            (true, false) => 0,
            (false, false) => {
                if self.truncate || self.create || self.create_new {
                    return Err(io::Error::from_raw_os_error(EINVAL));
                }
                0
            }
            (_, true) => {
                if self.truncate && !self.create_new {
                    return Err(io::Error::from_raw_os_error(EINVAL));
                }
                0
            }
        };
        let mut f = access | creation;
        if self.create_new {
            f |= c::O_CREAT | c::O_EXCL;
        } else {
            if self.create {
                f |= c::O_CREAT;
            }
            if self.truncate {
                f |= c::O_TRUNC;
            }
        }
        Ok(f)
    }
}

impl File {
    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        let p = cstr(path)?;
        // AROS `open` is variadic and reads the `mode` va_arg unconditionally, so
        // always pass it (ignored unless O_CREAT).
        let fd = unsafe { c::open(p.as_ptr(), opts.flags()?, 0o666 as crate::ffi::c_int) };
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
    pub fn truncate(&self, size: u64) -> io::Result<()> {
        if unsafe { c::ftruncate(self.fd, size as c::off_t) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    pub fn duplicate(&self) -> io::Result<File> {
        let fd = unsafe { c::dup(self.fd) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(File { fd }) }
    }
    pub fn set_permissions(&self, perm: FilePermissions) -> io::Result<()> {
        if unsafe { c::fchmod(self.fd, (perm.mode & 0o7777) as c::mode_t) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    // posixc has no futimes(), so times cannot be set through an open fd.
    pub fn set_times(&self, _times: FileTimes) -> io::Result<()> {
        Err(io::const_error!(
            io::ErrorKind::Unsupported,
            "setting file times through an open handle is not available on AROS (no futimes)"
        ))
    }
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

pub fn readdir(p: &Path) -> io::Result<ReadDir> {
    let path = cstr(p)?;
    let dir = unsafe { c::aros_opendir(path.as_ptr()) };
    if dir.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(ReadDir { dir, root: p.to_path_buf() })
    }
}

pub fn unlink(p: &Path) -> io::Result<()> {
    let c = cstr(p)?;
    if unsafe { c::unlink(c.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    let a = cstr(old)?;
    let b = cstr(new)?;
    if unsafe { c::rename(a.as_ptr(), b.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub fn set_perm(p: &Path, perm: FilePermissions) -> io::Result<()> {
    let path = cstr(p)?;
    if unsafe { c::chmod(path.as_ptr(), (perm.mode & 0o7777) as c::mode_t) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `filetime` for a `set_times` call. When a caller sets only one of atime/mtime,
/// std still hands us both fields; a `None` field means "leave unchanged", which
/// posixc `utimes` can't express (it rewrites both), so we fill the gap from the
/// current on-disk value via `stat` — matching what the unix pal does with
/// `UTIME_OMIT` when the platform lacks it.
fn set_times_common(p: &Path, times: FileTimes) -> io::Result<()> {
    let (at, mt) = match (times.accessed, times.modified) {
        (Some(a), Some(m)) => (a, m),
        _ => {
            let cur = stat(p)?;
            let a = times.accessed.map_or_else(|| cur.accessed(), Ok)?;
            let m = times.modified.map_or_else(|| cur.modified(), Ok)?;
            (a, m)
        }
    };
    let (as_, an) = at.to_secs_nanos();
    let (ms, mn) = mt.to_secs_nanos();
    let path = cstr(p)?;
    if unsafe { c::aros_utimes(path.as_ptr(), as_, an, ms, mn) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn set_times(p: &Path, times: FileTimes) -> io::Result<()> {
    set_times_common(p, times)
}

// posixc has no lutimes(), so a symlink's own times can't be set without following.
pub fn set_times_nofollow(_p: &Path, _times: FileTimes) -> io::Result<()> {
    Err(io::const_error!(
        io::ErrorKind::Unsupported,
        "setting a symlink's own times is not available on AROS (no lutimes)"
    ))
}

pub fn rmdir(p: &Path) -> io::Result<()> {
    let c = cstr(p)?;
    if unsafe { c::rmdir(c.as_ptr()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

// Portable recursive implementation over this backend's read_dir/remove.
pub use crate::sys::fs::common::remove_dir_all;
pub fn exists(path: &Path) -> io::Result<bool> {
    match stat(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
pub fn readlink(p: &Path) -> io::Result<PathBuf> {
    let path = cstr(p)?;
    // readlink doesn't NUL-terminate and gives no size hint; grow until it fits.
    // A symlink target is bounded by the filesystem (well under a few KB), so
    // cap the growth: a handler that keeps reporting "buffer too small" past
    // this is misbehaving (e.g. answering ACTION_READ_LINK on a non-symlink),
    // and unbounded growth would exhaust memory rather than fail the call.
    const MAX_CAP: usize = 64 * 1024;
    let mut cap = 256usize;
    loop {
        let mut buf: Vec<u8> = vec![0; cap];
        let n = unsafe {
            c::readlink(path.as_ptr(), buf.as_mut_ptr() as *mut crate::ffi::c_char, buf.len())
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            let os = unsafe { OsString::from_encoded_bytes_unchecked(buf) };
            return Ok(PathBuf::from(os));
        }
        if cap >= MAX_CAP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "readlink target exceeds the maximum symlink length",
            ));
        }
        cap = (cap * 2).min(MAX_CAP); // filled the buffer: the target may be longer, retry bigger
    }
}

pub fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    // posixc symlink(oldpath, newpath): oldpath is the link target, newpath the link.
    let target = cstr(original)?;
    let linkpath = cstr(link)?;
    if unsafe { c::symlink(target.as_ptr(), linkpath.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

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
pub fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    // Lock() + NameFromLock() via the glue: the handler resolves assigns,
    // `..` components, and symlinks, and returns the absolute
    // volume-rooted name.
    let path = cstr(p)?;
    let mut buf = [0u8; c::PATH_MAX];
    let r = unsafe {
        c::aros_realpath(path.as_ptr(), buf.as_mut_ptr() as *mut crate::ffi::c_char, buf.len())
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let os = unsafe { OsString::from_encoded_bytes_unchecked(buf[..len].to_vec()) };
    Ok(PathBuf::from(os))
}
// Portable open/read/write implementation over this backend's File.
pub use crate::sys::fs::common::copy;
