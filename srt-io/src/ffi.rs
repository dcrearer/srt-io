//! The **only** module in this crate that contains `unsafe` code.
//!
//! Everything here is a thin, safe-to-call wrapper around the raw `srt_io_sys`
//! FFI surface. Callers elsewhere in the crate get ordinary safe Rust: raw
//! C sockets are represented as [`RawSocket`] (a plain `i32`), pointers never
//! escape this module, and every fallible call is reduced to a
//! [`crate::Result`]. Each `unsafe` block carries a `// SAFETY:` note.
//!
//! Keeping `unsafe` confined here is a hard invariant of the crate: adding
//! `unsafe` anywhere else is a bug.

use std::ffi::CStr;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::raw::c_int;

use crate::error::{Error, Result};

/// Raw libsrt socket handle (`SRTSOCKET`).
pub type RawSocket = srt_io_sys::SRTSOCKET;

/// Sentinel returned by libsrt for an invalid socket.
pub const INVALID_SOCK: RawSocket = srt_io_sys::SRT_INVALID_SOCK;

// `sockaddr_in` / `sockaddr_in6` carry a leading `sin_len` / `sin6_len` byte
// only on BSD-derived platforms (macOS/iOS/*BSD). Those fields are set via the
// `#[cfg]`-gated blocks below; on other platforms (e.g. Linux) the fields do
// not exist and must not be referenced — hence compile-time `cfg`, not a
// runtime check.

/// Reduce a libsrt return code to a `Result`, mapping `SRT_ERROR` to the last
/// recorded error for `context`.
fn check(rc: c_int, context: &'static str) -> Result<()> {
    if rc == srt_io_sys::SRT_ERROR {
        Err(Error::from_last(context))
    } else {
        Ok(())
    }
}

/// Fetch libsrt's last error: `(code, message)`.
///
/// Used by [`crate::error::Error`] to build messages.
pub fn last_error() -> (i32, String) {
    let mut errno: c_int = 0;
    // SAFETY: `srt_getlasterror` writes the secondary errno through the
    // supplied pointer; `&mut errno` is a valid, aligned, writable location.
    let code = unsafe { srt_io_sys::srt_getlasterror(&mut errno) };
    // SAFETY: `srt_getlasterror_str` returns a pointer to a static,
    // NUL-terminated C string owned by libsrt; we only borrow it to copy.
    let message = unsafe {
        let ptr = srt_io_sys::srt_getlasterror_str();
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    (code as i32, message)
}

/// Fetch the connection reject reason for `sock`: `(code, description)`.
pub fn reject_reason(sock: RawSocket) -> (i32, String) {
    // SAFETY: `srt_getrejectreason` takes a socket id by value and has no
    // pointer arguments; any id is accepted and validated internally.
    let code = unsafe { srt_io_sys::srt_getrejectreason(sock) };
    // SAFETY: returns a pointer to a static NUL-terminated string owned by
    // libsrt; borrowed only for the duration of the copy.
    let message = unsafe {
        let ptr = srt_io_sys::srt_rejectreason_str(code);
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    (code as i32, message)
}

/// `srt_startup`. Reduced to `Ok(())` on success (libsrt returns `>= 0`).
pub fn startup() -> Result<()> {
    // SAFETY: `srt_startup` takes no arguments and is safe to call repeatedly;
    // libsrt reference-counts internally.
    let rc = unsafe { srt_io_sys::srt_startup() };
    if rc < 0 {
        Err(Error::startup(last_error().1))
    } else {
        Ok(())
    }
}

/// `srt_cleanup`. Best-effort teardown; errors are logged, not propagated,
/// because this runs from `Drop`.
pub fn cleanup() {
    // SAFETY: `srt_cleanup` takes no arguments; libsrt reference-counts and
    // only truly tears down on the final matching call.
    let rc = unsafe { srt_io_sys::srt_cleanup() };
    if rc < 0 {
        let (code, message) = last_error();
        tracing::error!(error.code = code, error.message = %message, "srt_cleanup failed");
    }
}

/// `srt_create_socket`.
pub fn create_socket() -> Result<RawSocket> {
    // SAFETY: `srt_create_socket` takes no arguments and returns a socket id
    // or `SRT_INVALID_SOCK`.
    let sock = unsafe { srt_io_sys::srt_create_socket() };
    if sock == INVALID_SOCK {
        Err(Error::from_last("create socket"))
    } else {
        Ok(sock)
    }
}

/// `srt_close`. Best-effort; logs on failure (called from `Drop`).
pub fn close(sock: RawSocket) {
    // SAFETY: `srt_close` takes a socket id by value; double-close is reported
    // via return code rather than being undefined.
    let rc = unsafe { srt_io_sys::srt_close(sock) };
    if rc == srt_io_sys::SRT_ERROR {
        let (code, message) = last_error();
        tracing::warn!(sock, error.code = code, error.message = %message, "srt_close failed");
    }
}

/// Set an integer-valued socket option.
pub fn set_flag_i32(sock: RawSocket, opt: srt_io_sys::SRT_SOCKOPT, value: i32) -> Result<()> {
    // SAFETY: integer options expect a pointer to a 4-byte `int`; `&value` is
    // valid and aligned, and we pass the matching length.
    let rc = unsafe {
        srt_io_sys::srt_setsockflag(
            sock,
            opt,
            (&value as *const i32).cast(),
            std::mem::size_of::<i32>() as c_int,
        )
    };
    check(rc, "set socket option")
}

/// Set a 64-bit integer-valued socket option (e.g. `SRTO_MAXBW`).
pub fn set_flag_i64(sock: RawSocket, opt: srt_io_sys::SRT_SOCKOPT, value: i64) -> Result<()> {
    // SAFETY: `int64_t` options expect a pointer to an 8-byte value; `&value`
    // is valid and aligned, and we pass the matching length.
    let rc = unsafe {
        srt_io_sys::srt_setsockflag(
            sock,
            opt,
            (&value as *const i64).cast(),
            std::mem::size_of::<i64>() as c_int,
        )
    };
    check(rc, "set socket option")
}

/// Set a boolean-valued socket option (e.g. `SRTO_TLPKTDROP`).
///
/// libsrt's boolean options accept a C `int` of `0`/`1` (the same path used by
/// `SRTO_RCVSYN`/`SRTO_SNDSYN`), so this forwards to [`set_flag_i32`].
pub fn set_flag_bool(sock: RawSocket, opt: srt_io_sys::SRT_SOCKOPT, value: bool) -> Result<()> {
    set_flag_i32(sock, opt, i32::from(value))
}

/// Set a byte-buffer socket option (e.g. the encryption passphrase).
pub fn set_flag_bytes(sock: RawSocket, opt: srt_io_sys::SRT_SOCKOPT, value: &[u8]) -> Result<()> {
    // SAFETY: buffer options expect `(ptr, len)`; `value` is a valid slice and
    // we pass its real length. libsrt copies the bytes before returning.
    let rc = unsafe {
        srt_io_sys::srt_setsockflag(sock, opt, value.as_ptr().cast(), value.len() as c_int)
    };
    check(rc, "set socket option")
}

/// Read a 32-bit integer-valued socket option (`srt_getsockflag`).
///
/// Used by tests to confirm an option was applied; the instantaneous stats use
/// [`bstats`].
#[cfg(test)]
pub fn get_flag_i32(sock: RawSocket, opt: srt_io_sys::SRT_SOCKOPT) -> Result<i32> {
    let mut value: i32 = 0;
    let mut len: c_int = std::mem::size_of::<i32>() as c_int;
    // SAFETY: integer options write a 4-byte `int` through `optval`; `&mut
    // value` is a valid, aligned, writable location and `len` matches its size.
    let rc = unsafe {
        srt_io_sys::srt_getsockflag(sock, opt, (&mut value as *mut i32).cast(), &mut len)
    };
    check(rc, "get socket option")?;
    Ok(value)
}

/// `srt_bistats` in instantaneous mode. Fills and returns the raw performance
/// monitor struct; `clear` resets the interval counters after reading.
///
/// The returned value is a plain `#[repr(C)]` POD struct (no borrowed pointers),
/// which the safe [`crate::stats::Stats`] wrapper then projects.
pub fn bstats(sock: RawSocket, clear: bool) -> Result<srt_io_sys::SRT_TRACEBSTATS> {
    // SAFETY: `SRT_TRACEBSTATS` is a POD byte buffer; an all-zero bit pattern is
    // a valid, empty value.
    let mut perf: srt_io_sys::SRT_TRACEBSTATS = unsafe { std::mem::zeroed() };
    // SAFETY: `perf` is a valid, writable `SRT_TRACEBSTATS` that outlives the
    // call; libsrt fills it in and retains no pointer. The final `1` selects the
    // instantaneous variant.
    let rc = unsafe { srt_io_sys::srt_bistats(sock, &mut perf, c_int::from(clear), 1) };
    check(rc, "bstats")?;
    Ok(perf)
}

/// `srt_bind`.
pub fn bind(sock: RawSocket, addr: &SocketAddr) -> Result<()> {
    let (storage, len) = to_storage(addr);
    // SAFETY: `storage` outlives the call and is a correctly initialised
    // `sockaddr_*`; `len` is its matching byte length.
    let rc = unsafe {
        srt_io_sys::srt_bind(
            sock,
            (&storage as *const srt_io_sys::sockaddr_storage).cast(),
            len,
        )
    };
    check(rc, "bind")
}

/// `srt_listen`.
pub fn listen(sock: RawSocket, backlog: i32) -> Result<()> {
    // SAFETY: `srt_listen` takes the socket id and backlog by value.
    let rc = unsafe { srt_io_sys::srt_listen(sock, backlog as c_int) };
    check(rc, "listen")
}

/// `srt_accept`. Blocks until a peer connects. Returns the accepted socket and
/// the peer address.
pub fn accept(sock: RawSocket) -> Result<(RawSocket, Option<SocketAddr>)> {
    let mut storage: srt_io_sys::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len: c_int = std::mem::size_of::<srt_io_sys::sockaddr_storage>() as c_int;
    // SAFETY: `storage`/`len` are valid in/out buffers sized for any
    // `sockaddr`; libsrt writes the peer address and the actual length.
    let accepted = unsafe {
        srt_io_sys::srt_accept(
            sock,
            (&mut storage as *mut srt_io_sys::sockaddr_storage).cast(),
            &mut len,
        )
    };
    if accepted == INVALID_SOCK {
        return Err(Error::from_last("accept"));
    }
    Ok((accepted, from_storage(&storage)))
}

/// `srt_connect`. Blocks until the handshake completes or fails. On failure the
/// reject reason is attached to the error.
pub fn connect(sock: RawSocket, addr: &SocketAddr) -> Result<()> {
    let (storage, len) = to_storage(addr);
    // SAFETY: `storage` is a valid `sockaddr_*` that outlives the call; `len`
    // matches its byte length.
    let rc = unsafe {
        srt_io_sys::srt_connect(
            sock,
            (&storage as *const srt_io_sys::sockaddr_storage).cast(),
            len,
        )
    };
    if rc == srt_io_sys::SRT_ERROR {
        Err(Error::from_last_with_reject("connect", sock))
    } else {
        Ok(())
    }
}

/// `srt_send`. Returns the number of bytes handed to libsrt.
pub fn send(sock: RawSocket, buf: &[u8]) -> Result<usize> {
    // SAFETY: `srt_send` reads `len` bytes from `buf`; the slice is valid for
    // that length and libsrt does not retain the pointer.
    let rc = unsafe { srt_io_sys::srt_send(sock, buf.as_ptr().cast(), buf.len() as c_int) };
    if rc == srt_io_sys::SRT_ERROR {
        Err(Error::from_last("send"))
    } else {
        Ok(rc as usize)
    }
}

/// `srt_recv`. Blocks until a message arrives. Returns the number of bytes
/// written into `buf`.
pub fn recv(sock: RawSocket, buf: &mut [u8]) -> Result<usize> {
    // SAFETY: `srt_recv` writes up to `len` bytes into `buf`; the slice is a
    // valid, writable region of that length.
    let rc = unsafe { srt_io_sys::srt_recv(sock, buf.as_mut_ptr().cast(), buf.len() as c_int) };
    if rc == srt_io_sys::SRT_ERROR {
        Err(Error::from_last("recv"))
    } else {
        Ok(rc as usize)
    }
}

/// `srt_getsockname` — the locally bound address.
pub fn local_addr(sock: RawSocket) -> Result<SocketAddr> {
    getname(sock, false)
}

/// `srt_getpeername` — the connected peer address.
pub fn peer_addr(sock: RawSocket) -> Result<SocketAddr> {
    getname(sock, true)
}

fn getname(sock: RawSocket, peer: bool) -> Result<SocketAddr> {
    let mut storage: srt_io_sys::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len: c_int = std::mem::size_of::<srt_io_sys::sockaddr_storage>() as c_int;
    let ptr = (&mut storage as *mut srt_io_sys::sockaddr_storage).cast();
    // SAFETY: `storage`/`len` are a valid in/out buffer sized for any
    // `sockaddr`; libsrt writes the address and its actual length.
    let rc = unsafe {
        if peer {
            srt_io_sys::srt_getpeername(sock, ptr, &mut len)
        } else {
            srt_io_sys::srt_getsockname(sock, ptr, &mut len)
        }
    };
    let context = if peer {
        "get peer address"
    } else {
        "get local address"
    };
    check(rc, context)?;
    from_storage(&storage)
        .ok_or_else(|| Error::address("libsrt returned an unknown address family"))
}

// ---- async epoll support (tokio feature only) ---------------------------
//
// These wrappers back the async reactor. They are the only place the
// `srt_epoll_*` family and `srt_getsockstate` are called; like everything else
// in this module they reduce raw FFI to safe `Result`s and carry `// SAFETY:`
// notes. Gated on the `tokio` feature so the default build pulls none of it in.

/// `SRT_EPOLL_IN`: readable / a connection is waiting to be accepted.
#[cfg(feature = "tokio")]
pub const EPOLL_IN: c_int = srt_io_sys::SRT_EPOLL_OPT::SRT_EPOLL_IN as c_int;
/// `SRT_EPOLL_OUT`: writable / (for a connecting socket) connected.
#[cfg(feature = "tokio")]
pub const EPOLL_OUT: c_int = srt_io_sys::SRT_EPOLL_OPT::SRT_EPOLL_OUT as c_int;
/// `SRT_EPOLL_ERR`: the socket has broken / errored.
#[cfg(feature = "tokio")]
pub const EPOLL_ERR: c_int = srt_io_sys::SRT_EPOLL_OPT::SRT_EPOLL_ERR as c_int;

/// libsrt error code for a would-block receive (`SRT_EASYNCRCV`).
#[cfg(feature = "tokio")]
pub const EASYNCRCV: i32 = srt_io_sys::SRT_ERRNO::SRT_EASYNCRCV as i32;
/// libsrt error code for a would-block send (`SRT_EASYNCSND`).
#[cfg(feature = "tokio")]
pub const EASYNCSND: i32 = srt_io_sys::SRT_ERRNO::SRT_EASYNCSND as i32;

/// A per-socket readiness event reported by [`epoll_uwait`].
#[cfg(feature = "tokio")]
pub type EpollEvent = srt_io_sys::SRT_EPOLL_EVENT;

/// Connection state of a socket (`srt_getsockstate`), reduced to a safe enum.
#[cfg(feature = "tokio")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SockStatus {
    Init,
    Opened,
    Listening,
    Connecting,
    Connected,
    Broken,
    Closing,
    Closed,
    NonExist,
}

/// `srt_getsockstate`.
#[cfg(feature = "tokio")]
pub fn getsockstate(sock: RawSocket) -> SockStatus {
    use srt_io_sys::SRT_SOCKSTATUS::*;
    // SAFETY: `srt_getsockstate` takes a socket id by value and has no pointer
    // arguments; it returns a plain enum value.
    let raw = unsafe { srt_io_sys::srt_getsockstate(sock) };
    match raw {
        SRTS_INIT => SockStatus::Init,
        SRTS_OPENED => SockStatus::Opened,
        SRTS_LISTENING => SockStatus::Listening,
        SRTS_CONNECTING => SockStatus::Connecting,
        SRTS_CONNECTED => SockStatus::Connected,
        SRTS_BROKEN => SockStatus::Broken,
        SRTS_CLOSING => SockStatus::Closing,
        SRTS_CLOSED => SockStatus::Closed,
        SRTS_NONEXIST => SockStatus::NonExist,
    }
}

/// `srt_epoll_create`. Returns the new epoll id.
#[cfg(feature = "tokio")]
pub fn epoll_create() -> Result<c_int> {
    // SAFETY: `srt_epoll_create` takes no arguments and returns an id (>= 0) or
    // `SRT_ERROR`.
    let eid = unsafe { srt_io_sys::srt_epoll_create() };
    if eid == srt_io_sys::SRT_ERROR {
        Err(Error::from_last("epoll create"))
    } else {
        Ok(eid)
    }
}

/// `srt_epoll_release`. Best-effort teardown; logs on failure (called from
/// `Drop`).
#[cfg(feature = "tokio")]
pub fn epoll_release(eid: c_int) {
    // SAFETY: `srt_epoll_release` takes the epoll id by value.
    let rc = unsafe { srt_io_sys::srt_epoll_release(eid) };
    if rc == srt_io_sys::SRT_ERROR {
        let (code, message) = last_error();
        tracing::warn!(eid, error.code = code, error.message = %message, "srt_epoll_release failed");
    }
}

/// `srt_epoll_add_usock` with the given event mask.
#[cfg(feature = "tokio")]
pub fn epoll_add_usock(eid: c_int, sock: RawSocket, events: c_int) -> Result<()> {
    // SAFETY: `events` points to a single valid `int` that outlives the call;
    // libsrt reads the mask and does not retain the pointer.
    let rc = unsafe { srt_io_sys::srt_epoll_add_usock(eid, sock, &events as *const c_int) };
    check(rc, "epoll add")
}

/// `srt_epoll_update_usock` — change the subscribed event mask.
#[cfg(feature = "tokio")]
pub fn epoll_update_usock(eid: c_int, sock: RawSocket, events: c_int) -> Result<()> {
    // SAFETY: `events` points to a single valid `int` that outlives the call.
    let rc = unsafe { srt_io_sys::srt_epoll_update_usock(eid, sock, &events as *const c_int) };
    check(rc, "epoll update")
}

/// `srt_epoll_remove_usock`.
#[cfg(feature = "tokio")]
pub fn epoll_remove_usock(eid: c_int, sock: RawSocket) -> Result<()> {
    // SAFETY: `srt_epoll_remove_usock` takes the epoll id and socket id by
    // value; removing an absent socket is reported via the return code.
    let rc = unsafe { srt_io_sys::srt_epoll_remove_usock(eid, sock) };
    check(rc, "epoll remove")
}

/// `srt_epoll_uwait`. Fills `events` with up to `events.len()` ready sockets and
/// returns how many were written. A return of `0` means the timeout elapsed.
#[cfg(feature = "tokio")]
pub fn epoll_uwait(eid: c_int, events: &mut [EpollEvent], timeout_ms: i64) -> Result<usize> {
    // SAFETY: `events` is a valid, writable slice; we pass its real length and
    // libsrt writes at most that many entries, returning the count.
    let rc = unsafe {
        srt_io_sys::srt_epoll_uwait(eid, events.as_mut_ptr(), events.len() as c_int, timeout_ms)
    };
    if rc == srt_io_sys::SRT_ERROR {
        Err(Error::from_last("epoll wait"))
    } else {
        Ok(rc as usize)
    }
}

/// Set a socket to libsrt's non-blocking mode (`SRTO_RCVSYN = SRTO_SNDSYN = 0`).
#[cfg(feature = "tokio")]
pub fn set_nonblocking(sock: RawSocket) -> Result<()> {
    use srt_io_sys::SRT_SOCKOPT::{SRTO_RCVSYN, SRTO_SNDSYN};
    set_flag_i32(sock, SRTO_SNDSYN, 0)?;
    set_flag_i32(sock, SRTO_RCVSYN, 0)?;
    Ok(())
}

// ---- address conversion -------------------------------------------------

/// Serialise a [`SocketAddr`] into a C `sockaddr_storage` plus its length.
///
/// We build the platform's `libc::sockaddr_in`/`sockaddr_in6` (zero-initialised
/// so absent fields such as BSD's `sin_len` compile everywhere) and copy its
/// bytes into the `srt_io_sys` storage, which has identical C layout.
fn to_storage(addr: &SocketAddr) -> (srt_io_sys::sockaddr_storage, c_int) {
    // SAFETY: `sockaddr_storage` is a POD byte buffer; an all-zero bit pattern
    // is a valid, empty value.
    let mut storage: srt_io_sys::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        SocketAddr::V4(v4) => {
            // SAFETY: `sockaddr_in` is POD; zeroed is a valid starting value.
            let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            #[cfg(any(
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "watchos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "dragonfly"
            ))]
            {
                sin.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            }
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = v4.port().to_be();
            // `octets()` are already in network byte order; interpreting them
            // in native order and storing yields the correct in-memory bytes.
            sin.sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
            let len = std::mem::size_of::<libc::sockaddr_in>();
            // SAFETY: copying `len` POD bytes from a valid `sockaddr_in` into a
            // larger storage buffer; regions do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&sin as *const libc::sockaddr_in).cast::<u8>(),
                    (&mut storage as *mut srt_io_sys::sockaddr_storage).cast::<u8>(),
                    len,
                );
            }
            (storage, len as c_int)
        }
        SocketAddr::V6(v6) => {
            // SAFETY: `sockaddr_in6` is POD; zeroed is a valid starting value.
            let mut sin6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            #[cfg(any(
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "watchos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "dragonfly"
            ))]
            {
                sin6.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            }
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_addr.s6_addr = v6.ip().octets();
            sin6.sin6_scope_id = v6.scope_id();
            let len = std::mem::size_of::<libc::sockaddr_in6>();
            // SAFETY: copying `len` POD bytes from a valid `sockaddr_in6` into a
            // larger storage buffer; regions do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&sin6 as *const libc::sockaddr_in6).cast::<u8>(),
                    (&mut storage as *mut srt_io_sys::sockaddr_storage).cast::<u8>(),
                    len,
                );
            }
            (storage, len as c_int)
        }
    }
}

/// Parse a C `sockaddr_storage` back into a [`SocketAddr`]. Returns `None` for
/// address families other than IPv4/IPv6.
fn from_storage(storage: &srt_io_sys::sockaddr_storage) -> Option<SocketAddr> {
    let family = storage.ss_family as i32;
    if family == libc::AF_INET {
        // SAFETY: the family tag says this storage holds a `sockaddr_in`;
        // reading its (smaller) POD layout out of the storage buffer is valid.
        let sin: libc::sockaddr_in = unsafe {
            let mut sin: libc::sockaddr_in = std::mem::zeroed();
            std::ptr::copy_nonoverlapping(
                (storage as *const srt_io_sys::sockaddr_storage).cast::<u8>(),
                (&mut sin as *mut libc::sockaddr_in).cast::<u8>(),
                std::mem::size_of::<libc::sockaddr_in>(),
            );
            sin
        };
        let ip = Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes());
        let port = u16::from_be(sin.sin_port);
        Some(SocketAddr::V4(SocketAddrV4::new(ip, port)))
    } else if family == libc::AF_INET6 {
        // SAFETY: the family tag says this storage holds a `sockaddr_in6`;
        // reading its POD layout out of the storage buffer is valid.
        let sin6: libc::sockaddr_in6 = unsafe {
            let mut sin6: libc::sockaddr_in6 = std::mem::zeroed();
            std::ptr::copy_nonoverlapping(
                (storage as *const srt_io_sys::sockaddr_storage).cast::<u8>(),
                (&mut sin6 as *mut libc::sockaddr_in6).cast::<u8>(),
                std::mem::size_of::<libc::sockaddr_in6>(),
            );
            sin6
        };
        let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
        let port = u16::from_be(sin6.sin6_port);
        Some(SocketAddr::V6(SocketAddrV6::new(
            ip,
            port,
            sin6.sin6_flowinfo,
            sin6.sin6_scope_id,
        )))
    } else {
        None
    }
}
