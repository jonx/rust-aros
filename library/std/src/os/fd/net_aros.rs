//! `AsFd`/`AsRawFd`/`FromRawFd`/`IntoRawFd` and `OwnedFd` conversions for the
//! network types on AROS.
//!
//! The AROS socket backend (`sys::net::connection::aros`) is a self-contained
//! bare-descriptor pal (`struct Socket(c_int)`), not the Berkeley `socket/`
//! backend the shared `os/fd/net.rs` bridge is written against. So the fd traits
//! are implemented here directly over the pal's raw-fd accessors.

use crate::os::fd::owned::{AsFd, BorrowedFd, OwnedFd};
use crate::os::fd::raw::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use crate::sys::{AsInner, FromInner, IntoInner};
use crate::{net, sys};

macro_rules! impl_fd_traits {
    ($($t:ident)*) => {$(
        #[stable(feature = "rust1", since = "1.0.0")]
        impl AsRawFd for net::$t {
            #[inline]
            fn as_raw_fd(&self) -> RawFd {
                self.as_inner().as_raw_fd()
            }
        }

        #[stable(feature = "from_raw_os", since = "1.1.0")]
        impl FromRawFd for net::$t {
            #[inline]
            unsafe fn from_raw_fd(fd: RawFd) -> net::$t {
                net::$t::from_inner(unsafe { sys::net::$t::from_raw_fd(fd) })
            }
        }

        #[stable(feature = "into_raw_os", since = "1.4.0")]
        impl IntoRawFd for net::$t {
            #[inline]
            fn into_raw_fd(self) -> RawFd {
                self.into_inner().into_raw_fd()
            }
        }

        #[stable(feature = "io_safety", since = "1.63.0")]
        impl AsFd for net::$t {
            #[inline]
            fn as_fd(&self) -> BorrowedFd<'_> {
                // SAFETY: the descriptor is owned by `self` and stays valid for
                // the borrow.
                unsafe { BorrowedFd::borrow_raw(self.as_raw_fd()) }
            }
        }

        #[stable(feature = "io_safety", since = "1.63.0")]
        impl From<net::$t> for OwnedFd {
            #[inline]
            fn from(value: net::$t) -> OwnedFd {
                // SAFETY: `into_raw_fd` surrenders ownership of a valid fd.
                unsafe { OwnedFd::from_raw_fd(value.into_raw_fd()) }
            }
        }

        #[stable(feature = "io_safety", since = "1.63.0")]
        impl From<OwnedFd> for net::$t {
            #[inline]
            fn from(owned: OwnedFd) -> net::$t {
                // SAFETY: `owned` hands over a valid, owned socket fd.
                unsafe { net::$t::from_raw_fd(owned.into_raw_fd()) }
            }
        }
    )*};
}

impl_fd_traits! { TcpStream TcpListener UdpSocket }
