//! Connected sockets and listeners.
//!
//! The inherent `send`/`recv`/`accept` methods here are **blocking** — they map
//! directly onto libsrt's synchronous mode (the default
//! `SRTO_RCVSYN`/`SRTO_SNDSYN`). With the `tokio` feature enabled, the same
//! [`Socket`]/[`Listener`] also gain `async` methods (`send_async`,
//! `recv_async`, `accept_async`) that flip the socket to non-blocking and drive
//! it with `srt_epoll`, added *alongside* the blocking ones without changing
//! their signatures. See the crate root docs.

use std::net::SocketAddr;
#[cfg(feature = "tokio")]
use std::sync::{Arc, OnceLock};

use crate::error::Result;
use crate::ffi::{self, RawSocket};
use crate::library::Library;
#[cfg(feature = "tokio")]
use crate::reactor::Reactor;
use crate::stats::Stats;

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

    /// Read this socket's transport [`Stats`] (`srt_bstats`, instantaneous
    /// variant). If `clear` is `true`, the interval counters are reset after
    /// the snapshot is taken.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn stats(&self, clear: bool) -> Result<Stats> {
        let raw = ffi::bstats(self.sock, clear).inspect_err(|e| {
            tracing::error!(error.kind = "stats", error = %e, "srt bstats failed");
        })?;
        Ok(Stats::from_raw(&raw))
    }

    // ---- post options (settable on a live, connected socket) -------------
    //
    // libsrt classifies these flags with the `post` restriction, meaning
    // `srt_setsockflag` accepts them after the connection is established. They
    // mirror the same-named [`Builder`](crate::Builder) setters, which apply the
    // value once at creation time; these let callers retune a running link.
    // Options with `pre`/`pre-bind` restriction are deliberately *not* exposed
    // here: libsrt rejects them on a connected socket, so changing one means
    // building a fresh connection.

    /// Set the maximum send bandwidth in bytes/sec (`SRTO_MAXBW`) on this live
    /// socket. `-1` means relative to the input rate; `0` means unlimited.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_max_bw(&self, bytes_per_sec: i64) -> Result<()> {
        self.set_i64(srt_io_sys::SRT_SOCKOPT::SRTO_MAXBW, bytes_per_sec)
    }

    /// Set the estimated input bandwidth in bytes/sec (`SRTO_INPUTBW`) on this
    /// live socket, used with [`set_overhead_bw`](Socket::set_overhead_bw) when
    /// `max_bw` is relative.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_input_bw(&self, bytes_per_sec: i64) -> Result<()> {
        self.set_i64(srt_io_sys::SRT_SOCKOPT::SRTO_INPUTBW, bytes_per_sec)
    }

    /// Set the overhead bandwidth as a percentage of input (`SRTO_OHEADBW`,
    /// 5–100) on this live socket.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_overhead_bw(&self, percent: i32) -> Result<()> {
        self.set_i32(srt_io_sys::SRT_SOCKOPT::SRTO_OHEADBW, percent)
    }

    /// Set the maximum reorder tolerance before a packet is declared lost
    /// (`SRTO_LOSSMAXTTL`, in packets) on this live socket.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_loss_max_ttl(&self, packets: i32) -> Result<()> {
        self.set_i32(srt_io_sys::SRT_SOCKOPT::SRTO_LOSSMAXTTL, packets)
    }

    /// Set the blocking-receive timeout (`SRTO_RCVTIMEO`, truncated to whole
    /// milliseconds) on this live socket.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_receive_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        self.set_i32(
            srt_io_sys::SRT_SOCKOPT::SRTO_RCVTIMEO,
            crate::options::duration_ms(timeout),
        )
    }

    /// Set the blocking-send timeout (`SRTO_SNDTIMEO`, truncated to whole
    /// milliseconds) on this live socket.
    #[tracing::instrument(level = "debug", skip(self), fields(sock = self.sock))]
    pub fn set_send_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        self.set_i32(
            srt_io_sys::SRT_SOCKOPT::SRTO_SNDTIMEO,
            crate::options::duration_ms(timeout),
        )
    }

    fn set_i32(&self, opt: srt_io_sys::SRT_SOCKOPT, value: i32) -> Result<()> {
        ffi::set_flag_i32(self.sock, opt, value).inspect_err(|e| {
            tracing::error!(error.kind = "set_option", error = %e, "srt set option failed");
        })
    }

    fn set_i64(&self, opt: srt_io_sys::SRT_SOCKOPT, value: i64) -> Result<()> {
        ffi::set_flag_i64(self.sock, opt, value).inspect_err(|e| {
            tracing::error!(error.kind = "set_option", error = %e, "srt set option failed");
        })
    }
}

#[cfg(test)]
impl Socket {
    /// Read back a raw `i32`-valued option, for test verification.
    pub(crate) fn get_flag_i32(&self, opt: srt_io_sys::SRT_SOCKOPT) -> Result<i32> {
        ffi::get_flag_i32(self.sock, opt)
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
