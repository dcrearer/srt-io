//! Safe Rust wrapper over libsrt (Secure Reliable Transport).
//!
//! This crate exposes a **blocking** SRT API built on top of the raw `srt-io-sys`
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
//! - [`Stats`] — a safe snapshot of a socket's transport statistics
//!   ([`Socket::stats`]).
//! - [`Error`] — the crate's single typed error, built with `thiserror` from
//!   libsrt's own diagnostics.
//!
//! # Example
//!
//! A blocking loopback round-trip: a listener echoes one message back to a
//! caller. Both ends come from the same [`Library`] handle.
//!
//! ```
//! use std::thread;
//! use std::time::Duration;
//! use srt_io::{Congestion, Library, TransportType};
//!
//! # fn main() -> srt_io::Result<()> {
//! let lib = Library::init()?;
//!
//! // Bind a listener on an ephemeral loopback port.
//! let listener = lib
//!     .builder()
//!     .transport_type(TransportType::Live)
//!     .congestion(Congestion::Live)
//!     .latency(Duration::from_millis(120))
//!     .bind("127.0.0.1:0")?;
//! listener.listen(1)?;
//! let addr = listener.local_addr()?;
//!
//! let server = thread::spawn(move || -> srt_io::Result<()> {
//!     let (conn, _peer) = listener.accept()?;
//!     let mut buf = [0u8; 1500];
//!     let n = conn.recv(&mut buf)?;
//!     conn.send(&buf[..n])?;
//!     // Keep the socket open until the client has read the echo.
//!     let _ = conn.recv(&mut buf);
//!     Ok(())
//! });
//!
//! let client = lib
//!     .builder()
//!     .transport_type(TransportType::Live)
//!     .connect_timeout(Duration::from_secs(3))
//!     .connect(addr)?;
//! client.send(b"hello over SRT")?;
//!
//! let mut buf = [0u8; 1500];
//! let n = client.recv(&mut buf)?;
//! assert_eq!(&buf[..n], b"hello over SRT");
//!
//! // `stats` exposes RTT, bandwidth, and packet/byte counters.
//! let stats = client.stats(false)?;
//! assert!(stats.packets_sent > 0);
//!
//! drop(client);
//! server.join().expect("server thread")?;
//! # Ok(())
//! # }
//! ```
//!
//! # Safety architecture
//!
//! All `unsafe` code is confined to the private `ffi` module. The crate
//! denies `unsafe_code` everywhere and relaxes that lint only for `ffi`, so
//! every other module is ordinary safe Rust that talks to libsrt solely
//! through `ffi`'s safe wrappers.
//!
//! # Async (the `tokio` feature)
//!
//! Enabling the optional `tokio` feature adds a fully asynchronous layer on the
//! *same* [`Socket`]/[`Listener`]/[`Builder`] types — no separate types, and no
//! change to the blocking signatures:
//!
//! - Blocking calls use libsrt's default synchronous mode
//!   (`SRTO_RCVSYN`/`SRTO_SNDSYN` enabled). Async sockets flip those flags to
//!   non-blocking and are driven by a shared `srt_epoll` reactor; the feature
//!   adds `async` methods (`connect_async`/`bind_async`, `accept_async`,
//!   `send_async`/`recv_async`) alongside the blocking ones.
//! - [`Builder`] separates configuration from socket creation, so the async
//!   constructors consume the identical configuration.
//! - [`Socket`] and [`Listener`] are `Send`, so they move onto a runtime's
//!   worker threads unchanged.
//!
//! The default build compiles none of this — the blocking API pulls in no Tokio.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod builder;
mod error;
mod library;
mod options;
mod socket;
mod stats;

// The Tokio async layer. Entirely additive and behind the `tokio` feature; the
// default build compiles none of it. See the crate root docs on the async plan.
#[cfg(feature = "tokio")]
mod reactor;

// `ffi` is the single module permitted to use `unsafe`; the crate-wide
// `forbid` above is relaxed only here.
#[allow(unsafe_code)]
mod ffi;

pub use builder::Builder;
pub use error::{Error, Result};
pub use library::Library;
pub use options::{Congestion, KeyLength, TransportType};
pub use socket::{Listener, Socket};
pub use stats::Stats;

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
    fn curated_options_roundtrip_and_apply() {
        // Exercise the curated option setters end to end, then confirm one of
        // them was actually pushed to libsrt by reading it back with
        // `srt_getsockflag` on the connected caller socket.
        let lib = Library::init().expect("startup");

        let configure = |b: Builder| {
            b.transport_type(TransportType::Live)
                .congestion(Congestion::Live)
                .max_bw(1_250_000)
                .input_bw(1_000_000)
                .overhead_bw(25)
                .send_buf(1_000_000)
                .recv_buf(1_000_000)
                .flow_window(25_600)
                .mss(1_500)
                .too_late_drop(true)
                .nak_report(true)
                .loss_max_ttl(16)
                .peer_idle_timeout(Duration::from_secs(10))
                .stream_id("test-stream")
                .retransmit_algo(1)
                .connect_timeout(Duration::from_secs(3))
                .receive_timeout(Duration::from_secs(5))
                .send_timeout(Duration::from_secs(5))
        };

        let listener = configure(lib.builder()).bind("127.0.0.1:0").expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        let server = thread::spawn(move || {
            let (conn, _peer) = listener.accept().expect("accept");
            let mut buf = [0u8; 1500];
            let n = conn.recv(&mut buf).expect("server recv");
            let _ = conn.send(&buf[..n]).expect("server echo");
            let mut drain = [0u8; 1500];
            let _ = conn.recv(&mut drain);
            drop(conn);
            drop(listener);
        });

        let client = configure(lib.builder()).connect(addr).expect("connect");

        // Verify a value survived the apply path (NAKREPORT is not renegotiated
        // by the handshake, so the caller-side value is stable).
        let nak = client
            .get_flag_i32(srt_io_sys::SRT_SOCKOPT::SRTO_NAKREPORT)
            .expect("getsockflag");
        assert_eq!(nak, 1, "nak_report(true) should read back as 1");

        let payload = b"curated options payload";
        client.send(payload).expect("client send");
        let mut buf = [0u8; 1500];
        let n = client.recv(&mut buf).expect("client recv");
        assert_eq!(&buf[..n], payload);

        drop(client);
        server.join().expect("server thread");
    }

    #[test]
    fn stats_report_nonzero_after_roundtrip() {
        let lib = Library::init().expect("startup");

        let listener = lib
            .builder()
            .transport_type(TransportType::Live)
            .bind("127.0.0.1:0")
            .expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        let server = thread::spawn(move || {
            let (conn, _peer) = listener.accept().expect("accept");
            let mut buf = [0u8; 1500];
            let n = conn.recv(&mut buf).expect("server recv");
            let _ = conn.send(&buf[..n]).expect("server echo");
            // Read stats on the receiving end: it must have received packets.
            let stats = conn.stats(false).expect("server stats");
            assert!(
                stats.packets_received > 0,
                "server should have received packets, got {stats:?}"
            );
            let mut drain = [0u8; 1500];
            let _ = conn.recv(&mut drain);
            drop(conn);
            drop(listener);
        });

        let client = lib
            .builder()
            .transport_type(TransportType::Live)
            .connect_timeout(Duration::from_secs(3))
            .connect(addr)
            .expect("connect");
        let payload = b"stats payload";
        client.send(payload).expect("client send");
        let mut buf = [0u8; 1500];
        let n = client.recv(&mut buf).expect("client recv");
        assert_eq!(&buf[..n], payload);

        let stats = client.stats(false).expect("client stats");
        assert!(
            stats.packets_sent > 0,
            "client should have sent packets, got {stats:?}"
        );
        assert!(
            stats.bytes_sent > 0,
            "client should have sent bytes, got {stats:?}"
        );

        drop(client);
        server.join().expect("server thread");
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

#[cfg(all(test, feature = "tokio"))]
mod async_tests {
    use super::*;
    use std::time::Duration;

    /// Fully asynchronous loopback round-trip: an async listener accepts an
    /// async caller, the caller sends, the server echoes, the caller reads it
    /// back. Every socket operation here is `_async` — nothing blocks the
    /// Tokio runtime.
    async fn async_roundtrip(
        configure: impl Fn(Builder) -> Builder + Send + Sync + Clone + 'static,
    ) {
        let lib = Library::init().expect("startup");

        let cfg_server = configure.clone();
        let listener = cfg_server(lib.builder())
            .bind_async("127.0.0.1:0")
            .await
            .expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        // Server task: accept, echo the first message, then keep the socket
        // open (via a trailing recv) until the client drops so the echoed
        // packet is not discarded from the client's playout buffer.
        let server = tokio::spawn(async move {
            let (conn, _peer) = listener.accept_async().await.expect("accept");
            let mut buf = [0u8; 1500];
            let n = conn.recv_async(&mut buf).await.expect("server recv");
            let sent = conn.send_async(&buf[..n]).await.expect("server echo");
            assert_eq!(sent, n);
            let mut drain = [0u8; 1500];
            let _ = conn.recv_async(&mut drain).await;
            drop(conn);
            drop(listener);
        });

        let client = configure(lib.builder())
            .connect_async(addr)
            .await
            .expect("connect");
        let payload = b"the quick brown fox jumps over the lazy dog";
        let sent = client.send_async(payload).await.expect("client send");
        assert_eq!(sent, payload.len());

        let mut buf = [0u8; 1500];
        let n = client.recv_async(&mut buf).await.expect("client recv");
        assert_eq!(&buf[..n], payload);

        // Dropping the client unblocks the server's trailing recv.
        drop(client);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn async_plaintext_roundtrip() {
        async_roundtrip(|b| {
            b.transport_type(TransportType::Live)
                .latency(Duration::from_millis(120))
                .connect_timeout(Duration::from_secs(3))
        })
        .await;
    }

    #[tokio::test]
    async fn async_encrypted_roundtrip() {
        async_roundtrip(|b| {
            b.transport_type(TransportType::Live)
                .key_length(KeyLength::Aes256)
                .passphrase("super-secret-passphrase")
                .connect_timeout(Duration::from_secs(3))
        })
        .await;
    }

    /// A rejected handshake (passphrase mismatch) surfaces through the async
    /// connect path as `ConnectionRejected`, without blocking the runtime.
    #[tokio::test]
    async fn async_wrong_passphrase_is_rejected() {
        let lib = Library::init().expect("startup");
        let listener = lib
            .builder()
            .transport_type(TransportType::Live)
            .key_length(KeyLength::Aes128)
            .passphrase("listener-passphrase")
            .bind_async("127.0.0.1:0")
            .await
            .expect("bind");
        listener.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local_addr");

        let result = lib
            .builder()
            .transport_type(TransportType::Live)
            .key_length(KeyLength::Aes128)
            .passphrase("caller-passphrase-xx")
            .connect_timeout(Duration::from_secs(3))
            .connect_async(addr)
            .await;

        assert!(
            matches!(result, Err(Error::ConnectionRejected { .. })),
            "expected ConnectionRejected, got {result:?}"
        );
        drop(listener);
    }
}
