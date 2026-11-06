//! Safe Rust wrapper over libsrt (Secure Reliable Transport).
//!
//! This crate exposes a **blocking** SRT API built on top of the raw `srt-sys`
//! FFI bindings:
//!
//! - [`Library`] — a reference-counted RAII guard around `srt_startup()` /
//!   `srt_cleanup()`. Sockets can only be created through a live `Library`, so
//!   the runtime is always initialised while sockets exist.
//! - [`Builder`] — fluent, strongly-typed configuration (latency, encryption,
//!   payload size, timeouts, transport type) that produces sockets.
//! - [`Socket`] — a connected caller/accepted socket with blocking
//!   [`send`](Socket::send) / [`recv`](Socket::recv).
//! - [`Listener`] — a bound socket with [`listen`](Listener::listen) /
//!   [`accept`](Listener::accept).
//! - [`Error`] — the crate's single typed error, built with `thiserror` from
//!   libsrt's own diagnostics.
//!
//! # Safety architecture
//!
//! All `unsafe` code is confined to the private [`ffi`] module. The crate
//! denies `unsafe_code` everywhere and relaxes that lint only for `ffi`, so
//! every other module is ordinary safe Rust that talks to libsrt solely
//! through `ffi`'s safe wrappers.
//!
//! # Forward-compatibility with async
//!
//! The public surface is deliberately designed so a future Tokio layer can be
//! added **without breaking changes**:
//!
//! - All blocking calls run in libsrt's default synchronous mode
//!   (`SRTO_RCVSYN`/`SRTO_SNDSYN` left enabled). An async layer will flip those
//!   flags to non-blocking and drive sockets with `srt_epoll`, but it will
//!   wrap the *same* [`Socket`]/[`Listener`]/[`Builder`] types and add `async`
//!   methods (e.g. `connect_async`, `send`/`recv` futures) alongside today's
//!   signatures rather than replacing them.
//! - [`Builder`] already separates configuration ([`crate::Builder`] options)
//!   from the act of creating a socket, so an async constructor can consume the
//!   identical configuration.
//! - [`Socket`] and [`Listener`] are `Send`, so they can move onto a runtime's
//!   worker threads unchanged.

#![deny(unsafe_code)]

mod builder;
mod error;
mod library;
mod options;
mod socket;

// `ffi` is the single module permitted to use `unsafe`; the crate-wide
// `forbid` above is relaxed only here.
#[allow(unsafe_code)]
mod ffi;

pub use builder::Builder;
pub use error::{Error, Result};
pub use library::Library;
pub use options::{KeyLength, TransportType};
pub use socket::{Listener, Socket};

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// Spawn a listener on an ephemeral loopback port, run `configure` on both
    /// ends, echo one message back, and assert the round-trip. Listener and
    /// caller run on separate threads because the API is blocking.
    fn roundtrip(configure: impl Fn(Builder) -> Builder + Send + Sync + 'static) {
        let lib = Library::init().expect("startup");

        let listener = configure(lib.builder()).bind("127.0.0.1:0").expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        // Move the already-bound listener into the server thread; it echoes
        // the first message straight back.
        let server = thread::spawn(move || {
            let (conn, _peer) = listener.accept().expect("accept");
            let mut buf = [0u8; 1500];
            let n = conn.recv(&mut buf).expect("server recv");
            let sent = conn.send(&buf[..n]).expect("server echo");
            assert_eq!(sent, n);
            // Stay open until the client finishes reading. If the server closed
            // now, the echoed packet still sitting in the client's TSBPD
            // playout buffer would be discarded as a broken connection. This
            // extra recv unblocks (with an error we ignore) once the client
            // drops its socket.
            let mut drain = [0u8; 1500];
            let _ = conn.recv(&mut drain);
            drop(conn);
            drop(listener);
        });

        let client = configure(lib.builder()).connect(addr).expect("connect");
        let payload = b"the quick brown fox jumps over the lazy dog";
        let sent = client.send(payload).expect("client send");
        assert_eq!(sent, payload.len());

        let mut buf = [0u8; 1500];
        let n = client.recv(&mut buf).expect("client recv");
        assert_eq!(&buf[..n], payload);

        // Closing the client unblocks the server's drain recv above.
        drop(client);
        server.join().expect("server thread");
    }

    #[test]
    fn library_init_is_refcounted() {
        let a = Library::init().expect("first init");
        let b = Library::init().expect("second init");
        let c = a.clone();
        drop(a);
        drop(b);
        drop(c);
        // Re-initialising after the count returns to zero must still work.
        let d = Library::init().expect("reinit after cleanup");
        drop(d);
    }

    #[test]
    fn plaintext_roundtrip() {
        roundtrip(|b| {
            b.transport_type(TransportType::Live)
                .latency(Duration::from_millis(120))
                .receive_timeout(Duration::from_secs(5))
                .send_timeout(Duration::from_secs(5))
                .connect_timeout(Duration::from_secs(3))
        });
    }

    #[test]
    fn encrypted_roundtrip() {
        roundtrip(|b| {
            b.transport_type(TransportType::Live)
                .key_length(KeyLength::Aes256)
                .passphrase("super-secret-passphrase")
                .receive_timeout(Duration::from_secs(5))
                .send_timeout(Duration::from_secs(5))
                .connect_timeout(Duration::from_secs(3))
        });
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let lib = Library::init().expect("startup");
        let listener = lib
            .builder()
            .transport_type(TransportType::Live)
            .key_length(KeyLength::Aes128)
            .passphrase("listener-passphrase")
            .bind("127.0.0.1:0")
            .expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        // No accept thread is needed: libsrt's listener worker rejects the
        // mismatched-passphrase handshake on its own, so `connect` returns the
        // rejection promptly. (`accept` would block forever here, since no
        // connection is ever established.) Keep the listener alive for the
        // duration of the attempt.
        let result = lib
            .builder()
            .transport_type(TransportType::Live)
            .key_length(KeyLength::Aes128)
            .passphrase("caller-passphrase-xx")
            .connect_timeout(Duration::from_secs(3))
            .connect(addr);

        assert!(
            matches!(result, Err(Error::ConnectionRejected { .. })),
            "expected ConnectionRejected, got {result:?}"
        );
        drop(listener);
    }
}
