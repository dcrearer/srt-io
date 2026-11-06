//! In-process FFI loopback interop tests.
//!
//! These prove OUR vendored libsrt (v1.5.4) interoperates with itself over a
//! real UDP socket on 127.0.0.1: a listener and a caller, each on its own
//! thread with blocking (synchronous) sockets, round-trip a payload. There is a
//! PLAINTEXT variant and an AES-ENCRYPTED variant (SRTO_PASSPHRASE +
//! SRTO_PBKEYLEN set on both ends).
//!
//! Run with: `cargo test -p srt-io-sys --test loopback`

mod common;

use std::thread;
use std::time::Duration;

use common::*;
use srt_io_sys::{
    SRT_INVALID_SOCK, srt_accept, srt_bind, srt_close, srt_connect, srt_create_socket, srt_listen,
    srt_recv, srt_send,
};

/// Encryption configuration for a loopback run.
#[derive(Clone, Copy)]
enum Crypto {
    Plaintext,
    Aes { keylen_bytes: i32 },
}

const PASSPHRASE: &[u8] = b"interop-test-passphrase-123";

/// Run one full listener/caller round-trip and assert the payload survives.
fn roundtrip(crypto: Crypto) {
    init();

    let payload = make_payload(1200);

    // --- Listener: bind to an ephemeral port, discover it, then listen. ---
    let listener = unsafe { srt_create_socket() };
    assert_ne!(
        listener,
        SRT_INVALID_SOCK,
        "create listener: {}",
        last_error()
    );
    set_blocking(listener);
    set_live(listener);
    if let Crypto::Aes { keylen_bytes } = crypto {
        set_encryption(listener, PASSPHRASE, keylen_bytes);
    }

    let bind_addr = sockaddr_in_v4([127, 0, 0, 1], 0);
    let rc = unsafe { srt_bind(listener, as_sockaddr_ptr(&bind_addr), sockaddr_len()) };
    assert_eq!(rc, 0, "srt_bind: {}", last_error());

    let port = local_port(listener);
    assert_ne!(port, 0, "ephemeral port should be non-zero");

    let rc = unsafe { srt_listen(listener, 1) };
    assert_eq!(rc, 0, "srt_listen: {}", last_error());

    // --- Receiver thread: accept a connection and drain one message. ---
    let expected_len = payload.len();
    let recv_thread = thread::spawn(move || {
        let accepted = unsafe { srt_accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_ne!(accepted, SRT_INVALID_SOCK, "srt_accept: {}", last_error());

        // In SRT live (message) mode the receive buffer must be able to hold a
        // full message (payload size, default 1316 bytes) or libsrt rejects the
        // call with "Incorrect use of Message API".
        let mut buf = vec![0u8; 1500];
        let _ = expected_len;
        let n = unsafe {
            srt_recv(
                accepted,
                buf.as_mut_ptr() as *mut std::os::raw::c_char,
                buf.len() as i32,
            )
        };
        assert!(n > 0, "srt_recv returned {n}: {}", last_error());
        buf.truncate(n as usize);

        unsafe {
            srt_close(accepted);
            srt_close(listener);
        }
        buf
    });

    // --- Caller: connect to the listener and send the payload. ---
    let caller = unsafe { srt_create_socket() };
    assert_ne!(caller, SRT_INVALID_SOCK, "create caller: {}", last_error());
    set_blocking(caller);
    set_live(caller);
    if let Crypto::Aes { keylen_bytes } = crypto {
        set_encryption(caller, PASSPHRASE, keylen_bytes);
    }

    let connect_addr = sockaddr_in_v4([127, 0, 0, 1], port);
    let rc = unsafe { srt_connect(caller, as_sockaddr_ptr(&connect_addr), sockaddr_len()) };
    assert_eq!(rc, 0, "srt_connect: {}", last_error());

    let sent = unsafe {
        srt_send(
            caller,
            payload.as_ptr() as *const std::os::raw::c_char,
            payload.len() as i32,
        )
    };
    assert_eq!(sent, payload.len() as i32, "srt_send: {}", last_error());

    // Give the live-mode sender buffer a moment to flush before we close.
    thread::sleep(Duration::from_millis(300));
    unsafe {
        srt_close(caller);
    }

    let received = recv_thread.join().expect("receiver thread panicked");
    assert_eq!(received, payload, "payload corrupted over the wire");
}

#[test]
fn loopback_plaintext() {
    roundtrip(Crypto::Plaintext);
}

#[test]
fn loopback_aes128() {
    roundtrip(Crypto::Aes { keylen_bytes: 16 });
}

#[test]
fn loopback_aes256() {
    roundtrip(Crypto::Aes { keylen_bytes: 32 });
}

/// Negative control: a caller WITHOUT the passphrase must fail to connect to an
/// encrypted listener. This proves encryption is actually enforced rather than
/// silently ignored.
#[test]
fn loopback_wrong_passphrase_rejected() {
    init();

    let listener = unsafe { srt_create_socket() };
    assert_ne!(listener, SRT_INVALID_SOCK);
    set_blocking(listener);
    set_live(listener);
    set_encryption(listener, PASSPHRASE, 16);

    let bind_addr = sockaddr_in_v4([127, 0, 0, 1], 0);
    assert_eq!(
        unsafe { srt_bind(listener, as_sockaddr_ptr(&bind_addr), sockaddr_len()) },
        0,
        "srt_bind: {}",
        last_error()
    );
    let port = local_port(listener);
    assert_eq!(
        unsafe { srt_listen(listener, 1) },
        0,
        "srt_listen: {}",
        last_error()
    );

    // Accept thread: either accept never completes or the connection is torn
    // down. We simply close the listener after a short grace period.
    let accept_thread = thread::spawn(move || {
        // In blocking mode accept would block forever on a rejected handshake,
        // so this thread just owns the listener handle for cleanup timing.
        thread::sleep(Duration::from_millis(1500));
        unsafe { srt_close(listener) };
    });

    let caller = unsafe { srt_create_socket() };
    assert_ne!(caller, SRT_INVALID_SOCK);
    set_blocking(caller);
    set_live(caller);
    // Wrong passphrase on purpose.
    set_encryption(caller, b"the-wrong-passphrase-entirely", 16);

    let connect_addr = sockaddr_in_v4([127, 0, 0, 1], port);
    let rc = unsafe { srt_connect(caller, as_sockaddr_ptr(&connect_addr), sockaddr_len()) };
    assert_ne!(
        rc, 0,
        "connect with wrong passphrase should have been rejected"
    );

    unsafe { srt_close(caller) };
    accept_thread.join().expect("accept thread panicked");
}
