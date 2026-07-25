//! AROS-specific extensions to primitives in the [`std::fs`] module.
//!
//! [`std::fs`]: crate::fs

#![stable(feature = "rust1", since = "1.0.0")]

use crate::fs::Metadata;
use crate::sys::AsInner;

/// AROS-specific extensions to [`fs::Metadata`].
///
/// [`fs::Metadata`]: crate::fs::Metadata
#[stable(feature = "rust1", since = "1.0.0")]
pub trait MetadataExt {
    /// Returns the file serial number.
    ///
    /// AROS derives this from a hash of the path rather than from an on-disk
    /// inode, so it identifies a file consistently but says nothing about
    /// storage layout, and two hard links to one file do not share it.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn ino(&self) -> u64;
    /// Returns the file mode, including the file-type bits.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn mode(&self) -> u32;
    /// Returns the number of hard links to the file.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn nlink(&self) -> u32;
    /// Returns the size of the file, in bytes.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn size(&self) -> u64;
    /// Returns the last access time, in seconds since the Unix epoch.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn atime(&self) -> i64;
    /// Returns the nanosecond part of the last access time.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn atime_nsec(&self) -> i64;
    /// Returns the last modification time, in seconds since the Unix epoch.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn mtime(&self) -> i64;
    /// Returns the nanosecond part of the last modification time.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn mtime_nsec(&self) -> i64;
    /// Returns the last status-change time, in seconds since the Unix epoch.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn ctime(&self) -> i64;
    /// Returns the nanosecond part of the last status-change time.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn ctime_nsec(&self) -> i64;
}

#[stable(feature = "rust1", since = "1.0.0")]
impl MetadataExt for Metadata {
    fn ino(&self) -> u64 {
        self.as_inner().ino()
    }
    fn mode(&self) -> u32 {
        self.as_inner().mode()
    }
    fn nlink(&self) -> u32 {
        self.as_inner().nlink()
    }
    fn size(&self) -> u64 {
        self.as_inner().size()
    }
    fn atime(&self) -> i64 {
        self.as_inner().atime().0
    }
    fn atime_nsec(&self) -> i64 {
        self.as_inner().atime().1
    }
    fn mtime(&self) -> i64 {
        self.as_inner().mtime().0
    }
    fn mtime_nsec(&self) -> i64 {
        self.as_inner().mtime().1
    }
    fn ctime(&self) -> i64 {
        self.as_inner().ctime().0
    }
    fn ctime_nsec(&self) -> i64 {
        self.as_inner().ctime().1
    }
}
