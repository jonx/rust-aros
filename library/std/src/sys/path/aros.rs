//! Path handling for AROS.
//!
//! Separators and components are unix-shaped, but what counts as *absolute* is
//! not. An AROS path is rooted at a named volume or assign rather than at a
//! single global root: `SYS:C/List`, `MacRW:project/src`, `PIPE:name`. There is
//! no `Prefix` variant that fits a multi-character volume name (the enum is
//! Windows-shaped, and `Prefix::Disk` holds one byte), so instead of inventing
//! a prefix this module answers `is_absolute` directly.
//!
//! Without this, `sys::path::unix::is_absolute` falls to
//! `has_root() && prefix().is_some()`, and since AROS parses no prefixes, *no*
//! path is ever absolute -- so every crate that branches on `is_absolute()`
//! silently treats `MacRW:proj` as relative and joins it onto the current
//! directory.

use crate::ffi::OsStr;
use crate::path::{Path, PathBuf, Prefix};
use crate::{env, io};

path_separator_bytes!(b'/');

#[inline]
pub const fn is_verbatim_sep(b: u8) -> bool {
    is_sep_byte(b)
}

#[inline]
pub fn parse_prefix(_: &OsStr) -> Option<Prefix<'_>> {
    // A volume is not a `Prefix`: the enum cannot represent a multi-character
    // name. `is_absolute` below recognises volumes without one.
    None
}

pub const HAS_PREFIXES: bool = false;

/// The byte offset just past a leading `VOLUME:`, if the path starts with one.
///
/// A volume name runs from the start of the path to the first `:`, and may not
/// contain a separator (`a/b:c` is a file called `b:c`, not a volume).
fn volume_len(path: &OsStr) -> Option<usize> {
    let bytes = path.as_encoded_bytes();
    let colon = bytes.iter().position(|&b| b == b':')?;
    if bytes[..colon].iter().any(|&b| is_sep_byte(b)) { None } else { Some(colon + 1) }
}

pub(crate) fn is_absolute(path: &Path) -> bool {
    // Either rooted at a volume, or at the separator (which AROS reads as "up
    // from here", but which callers writing `/x` mean as rooted).
    volume_len(path.as_os_str()).is_some() || path.has_root()
}

/// Make an AROS path absolute without changing its meaning.
pub(crate) fn absolute(path: &Path) -> io::Result<PathBuf> {
    if is_absolute(path) {
        // Already rooted at a volume: normalising the components would drop the
        // `VOLUME:` marker, so hand it back untouched.
        return Ok(path.to_path_buf());
    }
    let mut normalized = env::current_dir()?;
    normalized.extend(path.strip_prefix(".").unwrap_or(path).components());
    if path.as_os_str().as_encoded_bytes().ends_with(b"/") {
        normalized.push("");
    }
    Ok(normalized)
}
