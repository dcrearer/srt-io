//! Connected sockets and listeners.
//!
//! All operations here are **blocking** — they map directly onto libsrt's
//! synchronous mode (the default `SRTO_RCVSYN`/`SRTO_SNDSYN`). The types and
//! method names are chosen so a future Tokio layer can wrap the same
//! [`Socket`]/[`Listener`] (flipping the sockets to non-blocking and driving
//! them with `srt_epoll`) and add `async` methods *alongside* these without
//! changing or breaking the existing signatures. See the crate root docs.

use std::net::SocketAddr;

use crate::error::Result;
use crate::ffi::{self, RawSocket};
use crate::library::Library;

/// A connected SRT socket: the result of [`Builder::connect`](crate::Builder::connect)
/// or [`Listener::accept`].
///
/// Closed automatically on drop (`srt_close`).
#[derive(Debug)]
pub struct Socket {
    sock: RawSocket,
    // Keeps the libsrt runtime initialised for this socket's whole lifetime.
    _library: Library,
}

impl Socket {
    pub(crate) fn from_raw(sock: RawSocket, library: Library) -> Self {
        Socket {
            sock,
            _library: library,
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
        ffi::close(self.sock);
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
}

impl Listener {
    pub(crate) fn from_raw(sock: RawSocket, library: Library) -> Self {
        Listener { sock, library }
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
        ffi::close(self.sock);
    }
}
