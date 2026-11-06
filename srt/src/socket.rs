//! Connected sockets and listeners.
//!
//! All operations here are **blocking** — they map directly onto libsrt's
//! synchronous mode (the default `SRTO_RCVSYN`/`SRTO_SNDSYN`). The types and
//! method names are chosen so a future Tokio layer can wrap the same
//! [`Socket`]/[`Listener`] (flipping the sockets to non-blocking and driving
//! them with `srt_epoll`) and add `async` methods *alongside* these without
//! changing or breaking the existing signatures. See the crate root docs.

use std::net::SocketAddr;
#[cfg(feature = "tokio")]
use std::sync::{Arc, OnceLock};

use crate::error::Result;
use crate::ffi::{self, RawSocket};
use crate::library::Library;
#[cfg(feature = "tokio")]
use crate::reactor::Reactor;

/// A connected SRT socket: the result of [`Builder::connect`](crate::Builder::connect)
/// or [`Listener::accept`].
///
/// Closed automatically on drop (`srt_close`).
#[derive(Debug)]
pub struct Socket {
    sock: RawSocket,
    // Keeps the libsrt runtime initialised for this socket's whole lifetime.
    _library: Library,
    // The reactor this socket is registered with, once it is driven
    // asynchronously. Blocking sockets never populate this and are unaffected.
    #[cfg(feature = "tokio")]
    reactor: OnceLock<Arc<Reactor>>,
}

impl Socket {
    pub(crate) fn from_raw(sock: RawSocket, library: Library) -> Self {
        Socket {
            sock,
            _library: library,
            #[cfg(feature = "tokio")]
            reactor: OnceLock::new(),
        }
    }

    /// Send one message (blocking). Returns the number of bytes accepted.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(sock = self.sock, len = buf.len()))]
    pub fn send(&self, buf: &[u8]) -> Result<usize> {
        ffi::send(self.sock, buf).inspect_err(|e| {
            tracing::error!(error.kind = "send", error = %e, "srt send failed");
        })
    }

    /// Receive one message into `buf` (blocking). Returns the number of bytes
    /// written.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(sock = self.sock, cap = buf.len()))]
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        ffi::recv(self.sock, buf).inspect_err(|e| {
            tracing::error!(error.kind = "recv", error = %e, "srt recv failed");
        })
    }

    /// The local address this socket is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        ffi::local_addr(self.sock)
    }

    /// The address of the connected peer.
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        ffi::peer_addr(self.sock)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        #[cfg(feature = "tokio")]
        if let Some(reactor) = self.reactor.get() {
            reactor.deregister(self.sock);
        }
        ffi::close(self.sock);
    }
}

#[cfg(feature = "tokio")]
impl Socket {
    /// Build a socket that is already in non-blocking mode and registered with
    /// the shared reactor, ready for the async send/recv methods.
    pub(crate) fn from_raw_async(
        sock: RawSocket,
        library: Library,
        reactor: Arc<Reactor>,
    ) -> Result<Self> {
        ffi::set_nonblocking(sock)?;
        reactor.register(sock);
        let socket = Socket::from_raw(sock, library);
        let _ = socket.reactor.set(reactor);
        Ok(socket)
    }

    /// Obtain the reactor driving this socket, registering it (and flipping it
    /// to non-blocking mode) on first async use.
    pub(crate) fn reactor(&self) -> Result<&Arc<Reactor>> {
        if let Some(reactor) = self.reactor.get() {
            return Ok(reactor);
        }
        let reactor = Reactor::global()?;
        ffi::set_nonblocking(self.sock)?;
        reactor.register(self.sock);
        let _ = self.reactor.set(reactor);
        Ok(self.reactor.get().expect("reactor just set"))
    }

    /// Send one message asynchronously. Returns the number of bytes accepted.
    ///
    /// Resolves once libsrt accepts the message; while the send buffer is full
    /// the task is parked and woken by the reactor.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(sock = self.sock, len = buf.len()))]
    pub async fn send_async(&self, buf: &[u8]) -> Result<usize> {
        let reactor = self.reactor()?;
        loop {
            match ffi::send(self.sock, buf) {
                Err(crate::Error::Api { code, .. }) if code == ffi::EASYNCSND => {
                    reactor.clear_writable(self.sock);
                    std::future::poll_fn(|cx| reactor.poll_writable(self.sock, cx)).await;
                }
                other => {
                    return other.inspect_err(|e| {
                        tracing::error!(error.kind = "send", error = %e, "srt async send failed");
                    });
                }
            }
        }
    }

    /// Receive one message into `buf` asynchronously. Returns the number of
    /// bytes written.
    ///
    /// Resolves once a message is available; while the receive buffer is empty
    /// the task is parked and woken by the reactor.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(sock = self.sock, cap = buf.len()))]
    pub async fn recv_async(&self, buf: &mut [u8]) -> Result<usize> {
        let reactor = self.reactor()?;
        loop {
            match ffi::recv(self.sock, buf) {
                Err(crate::Error::Api { code, .. }) if code == ffi::EASYNCRCV => {
                    reactor.clear_readable(self.sock);
                    std::future::poll_fn(|cx| reactor.poll_readable(self.sock, cx)).await;
                }
                other => {
                    return other.inspect_err(|e| {
                        tracing::error!(error.kind = "recv", error = %e, "srt async recv failed");
                    });
                }
            }
        }
    }
}

/// A bound SRT listener. Create one with [`Builder::bind`](crate::Builder::bind),
/// then [`listen`](Listener::listen) and [`accept`](Listener::accept).
///
/// Closed automatically on drop (`srt_close`).
#[derive(Debug)]
pub struct Listener {
    sock: RawSocket,
    library: Library,
    #[cfg(feature = "tokio")]
    reactor: OnceLock<Arc<Reactor>>,
}

impl Listener {
    pub(crate) fn from_raw(sock: RawSocket, library: Library) -> Self {
        Listener {
            sock,
            library,
            #[cfg(feature = "tokio")]
            reactor: OnceLock::new(),
        }
    }

    /// Begin listening for incoming connections with the given backlog.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn listen(&self, backlog: i32) -> Result<()> {
        ffi::listen(self.sock, backlog).inspect_err(|e| {
            tracing::error!(error.kind = "listen", error = %e, "srt listen failed");
        })
    }

    /// Block until a peer connects, returning the accepted [`Socket`] and the
    /// peer's address (if libsrt reported a recognisable one).
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn accept(&self) -> Result<(Socket, Option<SocketAddr>)> {
        let (raw, peer) = ffi::accept(self.sock).inspect_err(|e| {
            tracing::error!(error.kind = "accept", error = %e, "srt accept failed");
        })?;
        Ok((Socket::from_raw(raw, self.library.clone()), peer))
    }

    /// The local address this listener is bound to (useful when binding to an
    /// ephemeral port 0).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        ffi::local_addr(self.sock)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(feature = "tokio")]
        if let Some(reactor) = self.reactor.get() {
            reactor.deregister(self.sock);
        }
        ffi::close(self.sock);
    }
}

#[cfg(feature = "tokio")]
impl Listener {
    pub(crate) fn reactor(&self) -> Result<&Arc<Reactor>> {
        if let Some(reactor) = self.reactor.get() {
            return Ok(reactor);
        }
        let reactor = Reactor::global()?;
        ffi::set_nonblocking(self.sock)?;
        reactor.register(self.sock);
        let _ = self.reactor.set(reactor);
        Ok(self.reactor.get().expect("reactor just set"))
    }

    /// Asynchronously accept the next incoming connection, returning the
    /// accepted [`Socket`] (itself async, non-blocking and reactor-registered)
    /// and the peer address.
    ///
    /// While no connection is pending the task is parked and woken by the
    /// reactor.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub async fn accept_async(&self) -> Result<(Socket, Option<SocketAddr>)> {
        let reactor = self.reactor()?;
        loop {
            match ffi::accept(self.sock) {
                Ok((raw, peer)) => {
                    let socket =
                        Socket::from_raw_async(raw, self.library.clone(), Arc::clone(reactor))?;
                    return Ok((socket, peer));
                }
                Err(crate::Error::Api { code, .. }) if code == ffi::EASYNCRCV => {
                    reactor.clear_readable(self.sock);
                    std::future::poll_fn(|cx| reactor.poll_readable(self.sock, cx)).await;
                }
                Err(e) => {
                    tracing::error!(error.kind = "accept", error = %e, "srt async accept failed");
                    return Err(e);
                }
            }
        }
    }
}
