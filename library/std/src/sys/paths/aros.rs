//! Path/cwd services for AROS, backed by posixc (`getcwd`/`chdir`).
//!
//! AROS is hosted here and has a real filesystem with a per-process current
//! directory, so `current_dir()` and `set_current_dir()` work; the rest
//! (PATH splitting, `current_exe`, `home_dir`) falls back to `unsupported`.

use crate::ffi::CString;
use crate::io;
use crate::path::{Path, PathBuf};

unsafe extern "C" {
    #[link_name = "getcwd"]
    fn c_getcwd(buf: *mut u8, size: usize) -> *mut u8;
    #[link_name = "chdir"]
    fn c_chdir(path: *const u8) -> i32;
}

pub fn getcwd() -> io::Result<PathBuf> {
    let mut buf = vec![0u8; 512];
    loop {
        let p = unsafe { c_getcwd(buf.as_mut_ptr(), buf.len()) };
        if !p.is_null() {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let s = String::from_utf8_lossy(&buf[..n]).into_owned();
            return Ok(PathBuf::from(s));
        }
        // Grow and retry on ERANGE; give up if it never fits.
        if buf.len() >= 16 * 1024 {
            return Err(io::Error::last_os_error());
        }
        buf.resize(buf.len() * 2, 0);
    }
}

pub fn chdir(p: &Path) -> io::Result<()> {
    let s = CString::new(p.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if unsafe { c_chdir(s.as_ptr() as *const u8) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn temp_dir() -> PathBuf {
    PathBuf::from("T:")
}
