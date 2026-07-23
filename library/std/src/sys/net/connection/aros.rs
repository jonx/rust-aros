//! TCP/UDP for AROS, over the host-passthrough `bsdsocket.library`.
//!
//! AROS sockets live in their own descriptor space (closed with `CloseSocket`,
//! not `close`; polled with `WaitSelect`, not `poll`), so the shared Berkeley
//! `socket/` backend — which is built on posixc `FileDesc`/`fcntl`/`poll` — does
//! not fit. This is a self-contained backend (like `motor`/`xous`) that drives the
//! library's LVOs through a thin C glue (`aros_net_glue.c`, the `aros_np_*`
//! functions), because the LVO stubs dispatch through `SocketBase` in a register
//! and can't be called from Rust directly.
//!
//! IPv4 only for now: the bridge is host-passthrough and BSD-identical to the Mac
//! for `AF_INET` plus the common socket options, but `AF_INET6` numbering differs,
//! so IPv6 requests return `Unsupported` rather than silently talking to the wrong
//! address family.
//!
//! Blocking is solid, and `try_clone`/`duplicate` work (BSD `Dup2Socket`). Two knobs
//! are wired but **not yet effective** because the AROS library keeps host sockets
//! `O_NONBLOCK` and emulates blocking with a timer-poll park: `set_nonblocking(true)`
//! (the library treats `FIONBIO` as a no-op) and read/write timeouts (we return
//! `Unsupported` for a requested timeout rather than lie). Making those effective needs
//! a `WaitSelect`-gated recv/send in the glue whose `fd_set` ABI must match the
//! host-passthrough library exactly; that is a disclosed tier-3 gap, not a pal bug.
//! Both are noted for upstream (UPSTREAM-NOTES #37).

#![allow(dead_code)]

use crate::ffi::{CStr, c_char, c_int, c_void};
use crate::io::{self, BorrowedCursor, ErrorKind, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::sys::helpers::run_with_cstr;
use crate::time::Duration;
use crate::{fmt, mem};

// -- the C glue (aros_net_glue.c), linked into the final AROS program ----------
unsafe extern "C" {
    fn aros_net_open() -> c_int;
    fn aros_sock_errno() -> c_int;
    fn aros_closesocket(s: c_int);
    fn aros_np_socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    fn aros_np_connect(s: c_int, addr_net: u32, port_net: u16) -> c_int;
    fn aros_np_bind(s: c_int, addr_net: u32, port_net: u16) -> c_int;
    fn aros_np_listen(s: c_int, backlog: c_int) -> c_int;
    fn aros_np_accept(s: c_int, addr_net: *mut u32, port_net: *mut u16) -> c_int;
    fn aros_np_send(s: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    fn aros_np_recv(s: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn aros_np_sendto(
        s: c_int,
        buf: *const c_void,
        len: usize,
        flags: c_int,
        addr_net: u32,
        port_net: u16,
    ) -> isize;
    fn aros_np_recvfrom(
        s: c_int,
        buf: *mut c_void,
        len: usize,
        flags: c_int,
        addr_net: *mut u32,
        port_net: *mut u16,
    ) -> isize;
    fn aros_np_getsockname(s: c_int, addr_net: *mut u32, port_net: *mut u16) -> c_int;
    fn aros_np_getpeername(s: c_int, addr_net: *mut u32, port_net: *mut u16) -> c_int;
    fn aros_np_shutdown(s: c_int, how: c_int) -> c_int;
    fn aros_np_setsockopt(s: c_int, level: c_int, name: c_int, val: *const c_void, len: u32)
    -> c_int;
    fn aros_np_getsockopt(s: c_int, level: c_int, name: c_int, val: *mut c_void, len: *mut u32)
    -> c_int;
    fn aros_np_set_nonblock(s: c_int, nonblock: c_int) -> c_int;
    fn aros_np_dup(s: c_int) -> c_int;
    fn aros_np_resolve4(name: *const c_char, out: *mut u32, max: c_int) -> c_int;
}

// -- BSD constants (host-identical for AF_INET + the common options) -----------
const AF_INET: c_int = 2;
const SOCK_STREAM: c_int = 1;
const SOCK_DGRAM: c_int = 2;
const MSG_PEEK: c_int = 0x2;

const SOL_SOCKET: c_int = 0xffff;
const SO_ERROR: c_int = 0x1007;
const SO_REUSEADDR: c_int = 0x0004;
const SO_BROADCAST: c_int = 0x0020;
const SO_KEEPALIVE: c_int = 0x0008;
const SO_LINGER: c_int = 0x0080;

const IPPROTO_IP: c_int = 0;
const IP_TTL: c_int = 4;
const IP_MULTICAST_TTL: c_int = 10;
const IP_MULTICAST_LOOP: c_int = 11;
const IP_ADD_MEMBERSHIP: c_int = 12;
const IP_DROP_MEMBERSHIP: c_int = 13;

const IPPROTO_TCP: c_int = 6;
const TCP_NODELAY: c_int = 1;

const SHUT_RD: c_int = 0;
const SHUT_WR: c_int = 1;
const SHUT_RDWR: c_int = 2;

// -- helpers -------------------------------------------------------------------

/// The bsdsocket per-task errno (AmiTCP numbering), mapped by `sys/io/error`.
fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { aros_sock_errno() })
}

fn unsupported_v6<T>() -> io::Result<T> {
    Err(io::const_error!(ErrorKind::Unsupported, "IPv6 is not supported by the AROS bsdsocket bridge yet"))
}

/// Open `bsdsocket.library` once for the process (idempotent in the glue).
fn ensure_lib() -> io::Result<()> {
    if unsafe { aros_net_open() } != 0 {
        return Err(io::const_error!(ErrorKind::Uncategorized, "cannot open bsdsocket.library"));
    }
    Ok(())
}

/// `SocketAddrV4` -> the (network-order u32, network-order u16) the glue takes.
fn v4_to_raw(a: &SocketAddrV4) -> (u32, u16) {
    (u32::from_ne_bytes(a.ip().octets()), a.port().to_be())
}

fn raw_to_v4(addr_net: u32, port_net: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(addr_net.to_ne_bytes()), u16::from_be(port_net))
}

fn addr_to_raw(addr: &SocketAddr) -> io::Result<(u32, u16)> {
    match addr {
        SocketAddr::V4(a) => Ok(v4_to_raw(a)),
        SocketAddr::V6(_) => unsupported_v6(),
    }
}

// -- Socket: the owned descriptor ----------------------------------------------

struct Socket(c_int);

impl Socket {
    fn new(ty: c_int) -> io::Result<Socket> {
        ensure_lib()?;
        let fd = unsafe { aros_np_socket(AF_INET, ty, 0) };
        if fd < 0 { Err(last_error()) } else { Ok(Socket(fd)) }
    }

    fn from_raw(fd: c_int) -> Socket {
        Socket(fd)
    }

    fn fd(&self) -> c_int {
        self.0
    }

    // Surrender ownership of the fd without closing it (for IntoRawFd).
    fn into_raw(self) -> c_int {
        let fd = self.0;
        core::mem::forget(self);
        fd
    }

    fn setsockopt<T>(&self, level: c_int, name: c_int, val: T) -> io::Result<()> {
        let r = unsafe {
            aros_np_setsockopt(
                self.0,
                level,
                name,
                (&raw const val).cast::<c_void>(),
                mem::size_of::<T>() as u32,
            )
        };
        if r < 0 { Err(last_error()) } else { Ok(()) }
    }

    fn getsockopt<T: Copy>(&self, level: c_int, name: c_int) -> io::Result<T> {
        unsafe {
            let mut val: T = mem::zeroed();
            let mut len = mem::size_of::<T>() as u32;
            let r = aros_np_getsockopt(self.0, level, name, (&raw mut val).cast::<c_void>(), &mut len);
            if r < 0 { Err(last_error()) } else { Ok(val) }
        }
    }

    fn recv_with_flags(&self, buf: &mut [u8], flags: c_int) -> io::Result<usize> {
        let n = unsafe {
            aros_np_recv(self.0, buf.as_mut_ptr().cast::<c_void>(), buf.len(), flags)
        };
        if n < 0 { Err(last_error()) } else { Ok(n as usize) }
    }

    fn send_with_flags(&self, buf: &[u8], flags: c_int) -> io::Result<usize> {
        let n = unsafe {
            aros_np_send(self.0, buf.as_ptr().cast::<c_void>(), buf.len(), flags)
        };
        if n < 0 { Err(last_error()) } else { Ok(n as usize) }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        let (mut a, mut p) = (0u32, 0u16);
        let r = unsafe { aros_np_getsockname(self.0, &mut a, &mut p) };
        if r < 0 { Err(last_error()) } else { Ok(SocketAddr::V4(raw_to_v4(a, p))) }
    }

    fn peer_addr(&self) -> io::Result<SocketAddr> {
        let (mut a, mut p) = (0u32, 0u16);
        let r = unsafe { aros_np_getpeername(self.0, &mut a, &mut p) };
        if r < 0 { Err(last_error()) } else { Ok(SocketAddr::V4(raw_to_v4(a, p))) }
    }

    fn take_error(&self) -> io::Result<Option<io::Error>> {
        let err: c_int = self.getsockopt(SOL_SOCKET, SO_ERROR)?;
        Ok(if err == 0 { None } else { Some(io::Error::from_raw_os_error(err)) })
    }

    fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        let r = unsafe { aros_np_set_nonblock(self.0, nb as c_int) };
        if r < 0 { Err(last_error()) } else { Ok(()) }
    }

    fn try_clone(&self) -> io::Result<Socket> {
        let fd = unsafe { aros_np_dup(self.0) };
        if fd < 0 { Err(last_error()) } else { Ok(Socket(fd)) }
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Read => SHUT_RD,
            Shutdown::Write => SHUT_WR,
            Shutdown::Both => SHUT_RDWR,
        };
        let r = unsafe { aros_np_shutdown(self.0, how) };
        if r < 0 { Err(last_error()) } else { Ok(()) }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        unsafe { aros_closesocket(self.0) };
    }
}

/// Requested socket timeouts can't be honoured yet (the library's blocking
/// emulation ignores them), so accept clearing (`None`) and reject a real value
/// instead of silently dropping it.
fn set_timeout(dur: Option<Duration>) -> io::Result<()> {
    match dur {
        None => Ok(()),
        Some(_) => Err(io::const_error!(
            ErrorKind::Unsupported,
            "socket timeouts are not yet supported on AROS (bsdsocket blocking is emulated)"
        )),
    }
}

// -- TcpStream -----------------------------------------------------------------

pub struct TcpStream(Socket);

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        super::each_addr(addr, TcpStream::connect_to)
    }

    fn connect_to(addr: &SocketAddr) -> io::Result<TcpStream> {
        let (a, p) = addr_to_raw(addr)?;
        let sock = Socket::new(SOCK_STREAM)?;
        let r = unsafe { aros_np_connect(sock.fd(), a, p) };
        if r < 0 { Err(last_error()) } else { Ok(TcpStream(sock)) }
    }

    /// The requested timeout can't be enforced by the library's blocking
    /// emulation, so this is a blocking connect (the host TCP connect timeout
    /// still bounds it); see the module note.
    pub fn connect_timeout(addr: &SocketAddr, _: Duration) -> io::Result<TcpStream> {
        TcpStream::connect_to(addr)
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        set_timeout(dur)
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        set_timeout(dur)
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv_with_flags(buf, MSG_PEEK)
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv_with_flags(buf, 0)
    }

    pub fn read_buf(&self, cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        io::default_read_buf(|b| self.read(b), cursor)
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

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        self.0.send_with_flags(buf, 0)
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

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }

    pub fn duplicate(&self) -> io::Result<TcpStream> {
        Ok(TcpStream(self.0.try_clone()?))
    }

    pub fn set_linger(&self, linger: Option<Duration>) -> io::Result<()> {
        let l = Linger {
            l_onoff: linger.is_some() as c_int,
            l_linger: linger.map_or(0, |d| d.as_secs() as c_int),
        };
        self.0.setsockopt(SOL_SOCKET, SO_LINGER, l)
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        let l: Linger = self.0.getsockopt(SOL_SOCKET, SO_LINGER)?;
        Ok((l.l_onoff != 0).then(|| Duration::from_secs(l.l_linger as u64)))
    }

    pub fn set_keepalive(&self, keepalive: bool) -> io::Result<()> {
        self.0.setsockopt(SOL_SOCKET, SO_KEEPALIVE, keepalive as c_int)
    }

    pub fn keepalive(&self) -> io::Result<bool> {
        let v: c_int = self.0.getsockopt(SOL_SOCKET, SO_KEEPALIVE)?;
        Ok(v != 0)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_TCP, TCP_NODELAY, nodelay as c_int)
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        let v: c_int = self.0.getsockopt(IPPROTO_TCP, TCP_NODELAY)?;
        Ok(v != 0)
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_IP, IP_TTL, ttl as c_int)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        let v: c_int = self.0.getsockopt(IPPROTO_IP, IP_TTL)?;
        Ok(v as u32)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.take_error()
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStream").field("fd", &self.0.fd()).finish()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Linger {
    l_onoff: c_int,
    l_linger: c_int,
}

// -- TcpListener ---------------------------------------------------------------

pub struct TcpListener(Socket);

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        super::each_addr(addr, TcpListener::bind_to)
    }

    fn bind_to(addr: &SocketAddr) -> io::Result<TcpListener> {
        let (a, p) = addr_to_raw(addr)?;
        let sock = Socket::new(SOCK_STREAM)?;
        // best-effort SO_REUSEADDR, matching the Berkeley backend's bind()
        let _ = sock.setsockopt(SOL_SOCKET, SO_REUSEADDR, 1 as c_int);
        if unsafe { aros_np_bind(sock.fd(), a, p) } < 0 {
            return Err(last_error());
        }
        if unsafe { aros_np_listen(sock.fd(), 128) } < 0 {
            return Err(last_error());
        }
        Ok(TcpListener(sock))
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (mut a, mut p) = (0u32, 0u16);
        let fd = unsafe { aros_np_accept(self.0.fd(), &mut a, &mut p) };
        if fd < 0 {
            return Err(last_error());
        }
        Ok((TcpStream(Socket::from_raw(fd)), SocketAddr::V4(raw_to_v4(a, p))))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        Ok(TcpListener(self.0.try_clone()?))
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_IP, IP_TTL, ttl as c_int)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        let v: c_int = self.0.getsockopt(IPPROTO_IP, IP_TTL)?;
        Ok(v as u32)
    }

    pub fn set_only_v6(&self, _: bool) -> io::Result<()> {
        unsupported_v6()
    }

    pub fn only_v6(&self) -> io::Result<bool> {
        // this is an IPv4 listener
        Ok(false)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.take_error()
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpListener").field("fd", &self.0.fd()).finish()
    }
}

// -- UdpSocket -----------------------------------------------------------------

pub struct UdpSocket(Socket);

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        super::each_addr(addr, UdpSocket::bind_to)
    }

    fn bind_to(addr: &SocketAddr) -> io::Result<UdpSocket> {
        let (a, p) = addr_to_raw(addr)?;
        let sock = Socket::new(SOCK_DGRAM)?;
        if unsafe { aros_np_bind(sock.fd(), a, p) } < 0 {
            return Err(last_error());
        }
        Ok(UdpSocket(sock))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.recvfrom_flags(buf, 0)
    }

    pub fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.recvfrom_flags(buf, MSG_PEEK)
    }

    fn recvfrom_flags(&self, buf: &mut [u8], flags: c_int) -> io::Result<(usize, SocketAddr)> {
        let (mut a, mut p) = (0u32, 0u16);
        let n = unsafe {
            aros_np_recvfrom(self.0.fd(), buf.as_mut_ptr().cast::<c_void>(), buf.len(), flags, &mut a, &mut p)
        };
        if n < 0 {
            return Err(last_error());
        }
        Ok((n as usize, SocketAddr::V4(raw_to_v4(a, p))))
    }

    pub fn send_to(&self, buf: &[u8], dst: &SocketAddr) -> io::Result<usize> {
        let (a, p) = addr_to_raw(dst)?;
        let n = unsafe {
            aros_np_sendto(self.0.fd(), buf.as_ptr().cast::<c_void>(), buf.len(), 0, a, p)
        };
        if n < 0 { Err(last_error()) } else { Ok(n as usize) }
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket(self.0.try_clone()?))
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        set_timeout(dur)
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        set_timeout(dur)
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_broadcast(&self, broadcast: bool) -> io::Result<()> {
        self.0.setsockopt(SOL_SOCKET, SO_BROADCAST, broadcast as c_int)
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        let v: c_int = self.0.getsockopt(SOL_SOCKET, SO_BROADCAST)?;
        Ok(v != 0)
    }

    pub fn set_multicast_loop_v4(&self, on: bool) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_IP, IP_MULTICAST_LOOP, on as u8)
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        let v: u8 = self.0.getsockopt(IPPROTO_IP, IP_MULTICAST_LOOP)?;
        Ok(v != 0)
    }

    pub fn set_multicast_ttl_v4(&self, ttl: u32) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_IP, IP_MULTICAST_TTL, ttl as u8)
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        let v: u8 = self.0.getsockopt(IPPROTO_IP, IP_MULTICAST_TTL)?;
        Ok(v as u32)
    }

    pub fn set_multicast_loop_v6(&self, _: bool) -> io::Result<()> {
        unsupported_v6()
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        unsupported_v6()
    }

    pub fn join_multicast_v4(&self, multiaddr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        let mreq = IpMreq {
            imr_multiaddr: u32::from_ne_bytes(multiaddr.octets()),
            imr_interface: u32::from_ne_bytes(interface.octets()),
        };
        self.0.setsockopt(IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq)
    }

    pub fn join_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported_v6()
    }

    pub fn leave_multicast_v4(&self, multiaddr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        let mreq = IpMreq {
            imr_multiaddr: u32::from_ne_bytes(multiaddr.octets()),
            imr_interface: u32::from_ne_bytes(interface.octets()),
        };
        self.0.setsockopt(IPPROTO_IP, IP_DROP_MEMBERSHIP, mreq)
    }

    pub fn leave_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported_v6()
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.0.setsockopt(IPPROTO_IP, IP_TTL, ttl as c_int)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        let v: c_int = self.0.getsockopt(IPPROTO_IP, IP_TTL)?;
        Ok(v as u32)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.take_error()
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv_with_flags(buf, 0)
    }

    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv_with_flags(buf, MSG_PEEK)
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.0.send_with_flags(buf, 0)
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        super::each_addr(addr, |a| self.connect_to(a))
    }

    fn connect_to(&self, addr: &SocketAddr) -> io::Result<()> {
        let (a, p) = addr_to_raw(addr)?;
        if unsafe { aros_np_connect(self.0.fd(), a, p) } < 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UdpSocket").field("fd", &self.0.fd()).finish()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IpMreq {
    imr_multiaddr: u32,
    imr_interface: u32,
}

// -- name resolution -----------------------------------------------------------

pub struct LookupHost {
    iter: crate::vec::IntoIter<SocketAddr>,
}

impl Iterator for LookupHost {
    type Item = SocketAddr;
    fn next(&mut self) -> Option<SocketAddr> {
        self.iter.next()
    }
}

pub fn lookup_host(host: &str, port: u16) -> io::Result<LookupHost> {
    ensure_lib()?;

    // An IPv4 literal needs no DNS.
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        let v = vec![SocketAddr::V4(SocketAddrV4::new(ip, port))];
        return Ok(LookupHost { iter: v.into_iter() });
    }

    run_with_cstr(host.as_bytes(), &|c_host: &CStr| {
        let mut raw = [0u32; 8];
        let n = unsafe { aros_np_resolve4(c_host.as_ptr(), raw.as_mut_ptr(), raw.len() as c_int) };
        if n < 0 {
            return Err(io::const_error!(ErrorKind::Uncategorized, "host lookup failed"));
        }
        let addrs: Vec<SocketAddr> = raw[..n as usize]
            .iter()
            .map(|&a| SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(a.to_ne_bytes()), port)))
            .collect();
        Ok(LookupHost { iter: addrs.into_iter() })
    })
}

// Raw-fd accessors used by the `std::os::fd` bridge for network types
// (see `os/fd/net_aros.rs`). The AROS socket layer is fd-based, so these
// simply expose / consume / wrap the underlying descriptor.
impl TcpStream {
    pub fn as_raw_fd(&self) -> c_int {
        self.0.fd()
    }
    pub fn into_raw_fd(self) -> c_int {
        self.0.into_raw()
    }
    pub unsafe fn from_raw_fd(fd: c_int) -> TcpStream {
        TcpStream(Socket::from_raw(fd))
    }
}

impl TcpListener {
    pub fn as_raw_fd(&self) -> c_int {
        self.0.fd()
    }
    pub fn into_raw_fd(self) -> c_int {
        self.0.into_raw()
    }
    pub unsafe fn from_raw_fd(fd: c_int) -> TcpListener {
        TcpListener(Socket::from_raw(fd))
    }
}

impl UdpSocket {
    pub fn as_raw_fd(&self) -> c_int {
        self.0.fd()
    }
    pub fn into_raw_fd(self) -> c_int {
        self.0.into_raw()
    }
    pub unsafe fn from_raw_fd(fd: c_int) -> UdpSocket {
        UdpSocket(Socket::from_raw(fd))
    }
}
