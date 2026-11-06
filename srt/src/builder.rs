//! Fluent configuration for SRT sockets.
//!
//! A [`Builder`] collects options with safe Rust types, then produces either a
//! connected [`Socket`] (as a caller) via [`connect`](Builder::connect) or a
//! bound [`Listener`] via [`bind`](Builder::bind). It carries its own
//! [`Library`] handle, so anything it produces keeps the runtime alive.
//!
//! The same builder is intended to back a future async layer unchanged: an
//! async constructor would consume the identical [`Options`] and return async
//! socket types.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::ffi;
use crate::library::Library;
use crate::options::{KeyLength, Options, TransportType};
use crate::socket::{Listener, Socket};

/// Builder for configuring and creating SRT sockets.
#[derive(Debug, Clone)]
pub struct Builder {
    library: Library,
    options: Options,
}

impl Builder {
    pub(crate) fn new(library: Library) -> Self {
        Builder {
            library,
            options: Options::default(),
        }
    }

    /// Transport profile (`SRTO_TRANSTYPE`): [`TransportType::Live`] or
    /// [`TransportType::File`]. Set this first; it resets other defaults.
    #[must_use]
    pub fn transport_type(mut self, ty: TransportType) -> Self {
        self.options.transport_type = Some(ty);
        self
    }

    /// Bidirectional latency (`SRTO_LATENCY`).
    #[must_use]
    pub fn latency(mut self, latency: Duration) -> Self {
        self.options.latency = Some(latency);
        self
    }

    /// Receiver-side latency (`SRTO_RCVLATENCY`).
    #[must_use]
    pub fn receive_latency(mut self, latency: Duration) -> Self {
        self.options.recv_latency = Some(latency);
        self
    }

    /// Announced peer latency (`SRTO_PEERLATENCY`).
    #[must_use]
    pub fn peer_latency(mut self, latency: Duration) -> Self {
        self.options.peer_latency = Some(latency);
        self
    }

    /// Encryption passphrase (`SRTO_PASSPHRASE`). Must be 10–79 bytes;
    /// validated when the socket is created.
    #[must_use]
    pub fn passphrase(mut self, passphrase: impl Into<String>) -> Self {
        self.options.passphrase = Some(passphrase.into());
        self
    }

    /// Encryption key length (`SRTO_PBKEYLEN`).
    #[must_use]
    pub fn key_length(mut self, key_length: KeyLength) -> Self {
        self.options.key_length = Some(key_length);
        self
    }

    /// Payload size in bytes (`SRTO_PAYLOADSIZE`).
    #[must_use]
    pub fn payload_size(mut self, size: usize) -> Self {
        self.options.payload_size = Some(size);
        self
    }

    /// Blocking-receive timeout (`SRTO_RCVTIMEO`).
    #[must_use]
    pub fn receive_timeout(mut self, timeout: Duration) -> Self {
        self.options.recv_timeout = Some(timeout);
        self
    }

    /// Blocking-send timeout (`SRTO_SNDTIMEO`).
    #[must_use]
    pub fn send_timeout(mut self, timeout: Duration) -> Self {
        self.options.send_timeout = Some(timeout);
        self
    }

    /// Connection timeout (`SRTO_CONNTIMEO`).
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.options.connect_timeout = Some(timeout);
        self
    }

    /// Create a socket, apply all configured options, and block connecting to
    /// `addr` as a caller. Returns the connected [`Socket`].
    #[tracing::instrument(level = "debug", skip(self, addr))]
    pub fn connect(&self, addr: impl ToSocketAddrs) -> Result<Socket> {
        let addr = resolve(addr)?;
        let sock = ffi::create_socket()?;
        // Wrap immediately so the socket is closed on any early return.
        let socket = Socket::from_raw(sock, self.library.clone());
        if let Err(e) = self.options.apply(sock) {
            tracing::error!(error.kind = "configure", error = %e, "applying socket options failed");
            return Err(e);
        }
        if let Err(e) = ffi::connect(sock, &addr) {
            tracing::error!(error.kind = "connect", error = %e, %addr, "srt connect failed");
            return Err(e);
        }
        Ok(socket)
    }

    /// Create a socket, apply all configured options, and bind it to `addr`.
    /// Returns a [`Listener`]; call [`Listener::listen`] next.
    #[tracing::instrument(level = "debug", skip(self, addr))]
    pub fn bind(&self, addr: impl ToSocketAddrs) -> Result<Listener> {
        let addr = resolve(addr)?;
        let sock = ffi::create_socket()?;
        let listener = Listener::from_raw(sock, self.library.clone());
        if let Err(e) = self.options.apply(sock) {
            tracing::error!(error.kind = "configure", error = %e, "applying socket options failed");
            return Err(e);
        }
        if let Err(e) = ffi::bind(sock, &addr) {
            tracing::error!(error.kind = "bind", error = %e, %addr, "srt bind failed");
            return Err(e);
        }
        Ok(listener)
    }
}

#[cfg(feature = "tokio")]
impl Builder {
    /// Asynchronously create a non-blocking socket, apply all configured
    /// options, and connect to `addr` as a caller. Resolves to the connected
    /// [`Socket`], which is registered with the reactor for async I/O.
    ///
    /// Unlike [`connect`](Builder::connect) this never blocks the runtime: the
    /// handshake is driven by the reactor and the task is parked until the
    /// connection is established (or rejected).
    #[tracing::instrument(level = "debug", skip(self, addr))]
    pub async fn connect_async(&self, addr: impl ToSocketAddrs) -> Result<Socket> {
        use crate::ffi::SockStatus;

        let addr = resolve(addr)?;
        let sock = ffi::create_socket()?;
        // Wrap immediately so the socket is closed on any early return.
        let socket = Socket::from_raw(sock, self.library.clone());
        if let Err(e) = self.options.apply(sock) {
            tracing::error!(error.kind = "configure", error = %e, "applying socket options failed");
            return Err(e);
        }
        // Flip to non-blocking and register before connecting so the handshake
        // can be driven asynchronously.
        let reactor = std::sync::Arc::clone(socket.reactor()?);
        if let Err(e) = ffi::connect(sock, &addr) {
            tracing::error!(error.kind = "connect", error = %e, %addr, "srt async connect failed");
            return Err(e);
        }
        loop {
            match ffi::getsockstate(sock) {
                SockStatus::Connected => return Ok(socket),
                SockStatus::Broken | SockStatus::Closed | SockStatus::NonExist => {
                    let e = Error::from_reject("connect", sock);
                    tracing::error!(error.kind = "connect", error = %e, %addr, "srt async connect rejected");
                    return Err(e);
                }
                _ => {}
            }
            std::future::poll_fn(|cx| reactor.poll_writable(sock, cx)).await;
            reactor.clear_writable(sock);
        }
    }

    /// Asynchronously create a non-blocking socket, apply all configured
    /// options, and bind it to `addr`. Returns a [`Listener`] registered with
    /// the reactor; call [`Listener::listen`] then
    /// [`Listener::accept_async`].
    #[tracing::instrument(level = "debug", skip(self, addr))]
    pub async fn bind_async(&self, addr: impl ToSocketAddrs) -> Result<Listener> {
        let addr = resolve(addr)?;
        let sock = ffi::create_socket()?;
        let listener = Listener::from_raw(sock, self.library.clone());
        if let Err(e) = self.options.apply(sock) {
            tracing::error!(error.kind = "configure", error = %e, "applying socket options failed");
            return Err(e);
        }
        // Flip to non-blocking and register for async accept.
        listener.reactor()?;
        if let Err(e) = ffi::bind(sock, &addr) {
            tracing::error!(error.kind = "bind", error = %e, %addr, "srt async bind failed");
            return Err(e);
        }
        Ok(listener)
    }
}

/// Resolve `impl ToSocketAddrs` to its first address.
fn resolve(addr: impl ToSocketAddrs) -> Result<SocketAddr> {
    addr.to_socket_addrs()
        .map_err(|e| Error::address(format!("could not resolve address: {e}")))?
        .next()
        .ok_or_else(|| Error::address("address resolved to no entries"))
}
