//! Shared FFI test helpers for the libsrt interop harness.
//!
//! Everything here operates purely at the `srt_io_sys` raw FFI level so the tests
//! are independent of the safe `srt` crate. All functions are `unsafe` to call
//! in spirit, but the thin wrappers encapsulate the pointer juggling.
#![allow(dead_code)]

use std::ffi::CStr;
use std::sync::Once;

use srt_io_sys::SRT_SOCKOPT::{
    SRTO_PASSPHRASE, SRTO_PBKEYLEN, SRTO_RCVSYN, SRTO_SNDSYN, SRTO_TRANSTYPE,
};
use srt_io_sys::{
    SRT_SOCKOPT, SRT_TRANSTYPE, SRTSOCKET, srt_cleanup, srt_getlasterror_str, srt_getsockname,
    srt_setsockflag, srt_startup,
};

static START: Once = Once::new();

/// Initialise libsrt exactly once for the whole test binary.
///
/// `srt_startup`/`srt_cleanup` are process-global and reference counted. Because
/// cargo runs every `#[test]` as a thread inside one process, we start once and
/// register an `atexit` handler to call `srt_cleanup` at process exit — after
/// the harness has joined all test threads. This is necessary: without it,
/// libsrt's background GC thread is still running when the process tears down,
/// which segfaults on Linux (exit-time use-after-free). Doing cleanup from
/// `atexit` (registered after libsrt's own statics) gives an orderly shutdown.
pub fn init() {
    START.call_once(|| {
        let rc = unsafe { srt_startup() };
        assert!(rc >= 0, "srt_startup failed: {}", last_error());

        extern "C" fn cleanup_at_exit() {
            // SAFETY: runs at process exit once all test threads have been
            // joined, so no socket is in flight; `srt_cleanup` joins libsrt's
            // GC thread for an orderly teardown.
            unsafe {
                srt_cleanup();
            }
        }
        // SAFETY: registering a plain `extern "C"` function with libc's atexit.
        unsafe {
            libc::atexit(cleanup_at_exit);
        }
    });
}

/// The human-readable text of the most recent libsrt error on this thread.
pub fn last_error() -> String {
    unsafe {
        CStr::from_ptr(srt_getlasterror_str())
            .to_string_lossy()
            .into_owned()
    }
}

/// Build a macOS/BSD `sockaddr_in` for an IPv4 address + port.
///
/// On Darwin `sockaddr_in` carries a `sin_len` byte, which we set. The returned
/// value is layout-compatible with the bindgen `sockaddr` and is passed to
/// `srt_bind`/`srt_connect` via a pointer cast (see [`as_sockaddr_ptr`]).
pub fn sockaddr_in_v4(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
    // SAFETY: all-zero is a valid sockaddr_in.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
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
        sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = port.to_be(); // network byte order
    // s_addr holds the 4 address octets in network byte order.
    sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
    sa
}

/// Cast a `sockaddr_in` to the bindgen `*const sockaddr` expected by the FFI.
pub fn as_sockaddr_ptr(sa: &libc::sockaddr_in) -> *const srt_io_sys::sockaddr {
    sa as *const libc::sockaddr_in as *const srt_io_sys::sockaddr
}

pub fn sockaddr_len() -> i32 {
    std::mem::size_of::<libc::sockaddr_in>() as i32
}

/// Set an `i32`-valued socket flag, panicking with libsrt's error text on failure.
pub fn set_i32(sock: SRTSOCKET, opt: SRT_SOCKOPT, val: i32) {
    let rc = unsafe {
        srt_setsockflag(
            sock,
            opt,
            &val as *const i32 as *const std::os::raw::c_void,
            std::mem::size_of::<i32>() as i32,
        )
    };
    assert_eq!(rc, 0, "srt_setsockflag({opt:?}) failed: {}", last_error());
}

/// Set a byte-string socket flag (e.g. the passphrase).
pub fn set_bytes(sock: SRTSOCKET, opt: SRT_SOCKOPT, val: &[u8]) {
    let rc = unsafe {
        srt_setsockflag(
            sock,
            opt,
            val.as_ptr() as *const std::os::raw::c_void,
            val.len() as i32,
        )
    };
    assert_eq!(rc, 0, "srt_setsockflag({opt:?}) failed: {}", last_error());
}

/// Force SRT live transport type (the default, set explicitly for clarity).
pub fn set_live(sock: SRTSOCKET) {
    let tt = SRT_TRANSTYPE::SRTT_LIVE;
    let rc = unsafe {
        srt_setsockflag(
            sock,
            SRTO_TRANSTYPE,
            &tt as *const SRT_TRANSTYPE as *const std::os::raw::c_void,
            std::mem::size_of::<SRT_TRANSTYPE>() as i32,
        )
    };
    assert_eq!(rc, 0, "set SRTO_TRANSTYPE failed: {}", last_error());
}

/// Configure blocking (synchronous) send + receive on a socket.
pub fn set_blocking(sock: SRTSOCKET) {
    set_i32(sock, SRTO_RCVSYN, 1);
    set_i32(sock, SRTO_SNDSYN, 1);
}

/// Apply AES encryption to a socket: passphrase + key length in bytes.
pub fn set_encryption(sock: SRTSOCKET, passphrase: &[u8], keylen_bytes: i32) {
    set_bytes(sock, SRTO_PASSPHRASE, passphrase);
    set_i32(sock, SRTO_PBKEYLEN, keylen_bytes);
}

/// Read back the UDP port libsrt actually bound to (used with ephemeral port 0).
pub fn local_port(sock: SRTSOCKET) -> u16 {
    // SAFETY: zeroed sockaddr_in is valid; we pass its true size.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len: i32 = std::mem::size_of::<libc::sockaddr_in>() as i32;
    let rc = unsafe {
        srt_getsockname(
            sock,
            &mut sa as *mut libc::sockaddr_in as *mut srt_io_sys::sockaddr,
            &mut len,
        )
    };
    assert_eq!(rc, 0, "srt_getsockname failed: {}", last_error());
    u16::from_be(sa.sin_port)
}

/// A deterministic, non-trivial payload of `len` bytes for integrity checks.
pub fn make_payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + 7) & 0xff) as u8).collect()
}
