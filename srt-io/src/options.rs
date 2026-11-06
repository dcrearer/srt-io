//! Safe-typed socket configuration values and their mapping to raw libsrt
//! flags.
//!
//! Callers use Rust types ([`std::time::Duration`], the enums below); this
//! module records them and [`Options::apply`] translates them into
//! `srt_setsockflag` calls (via the safe [`crate::ffi`] layer) at socket
//! creation time.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::ffi::{self, RawSocket};

/// Transport profile (`SRTO_TRANSTYPE`).
///
/// This must be applied before any other option because libsrt resets several
/// defaults when it changes; `Options::apply` handles that ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    /// Live streaming profile (`SRTT_LIVE`) — low latency, message-oriented.
    Live,
    /// File/buffer transfer profile (`SRTT_FILE`) — throughput-oriented.
    File,
}

impl TransportType {
    fn as_raw(self) -> i32 {
        match self {
            TransportType::Live => srt_io_sys::SRT_TRANSTYPE::SRTT_LIVE as i32,
            TransportType::File => srt_io_sys::SRT_TRANSTYPE::SRTT_FILE as i32,
        }
    }
}

/// AES key length for encryption (`SRTO_PBKEYLEN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyLength {
    /// AES-128 (16-byte key).
    Aes128,
    /// AES-192 (24-byte key).
    Aes192,
    /// AES-256 (32-byte key).
    Aes256,
}

impl KeyLength {
    fn as_raw(self) -> i32 {
        match self {
            KeyLength::Aes128 => 16,
            KeyLength::Aes192 => 24,
            KeyLength::Aes256 => 32,
        }
    }
}

/// Congestion-control profile (`SRTO_CONGESTION`).
///
/// Marked `#[non_exhaustive]`: libsrt may gain further controllers, which can be
/// added here without breaking callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Congestion {
    /// The `"live"` controller — the default for the live transport profile.
    Live,
    /// The `"file"` controller — for the file/buffer transport profile.
    File,
}

impl Congestion {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Congestion::Live => "live",
            Congestion::File => "file",
        }
    }
}

/// A libsrt socket option this crate knows how to set.
///
/// This is the key of the crate's extensible option mechanism: each variant
/// names one `SRTO_*` flag and is paired with an [`OptValue`] of the matching
/// type in [`Options::extra`]. Adding support for a *new* libsrt option is a
/// non-breaking, two-line change — add one variant here (plus its `as_raw`
/// arm) and one fluent setter on [`Builder`](crate::Builder) that pushes
/// `(SockOpt::New, OptValue::…)`. The enum is `#[non_exhaustive]` to keep that
/// future growth from being a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum SockOpt {
    MaxBw,
    InputBw,
    OverheadBw,
    SendBuf,
    RecvBuf,
    FlowWindow,
    Mss,
    TooLateDrop,
    NakReport,
    LossMaxTtl,
    PeerIdleTimeout,
    StreamId,
    KmRefreshRate,
    KmPreannounce,
    Congestion,
    RetransmitAlgo,
}

impl SockOpt {
    /// The raw libsrt flag this option maps onto.
    fn as_raw(self) -> srt_io_sys::SRT_SOCKOPT {
        use srt_io_sys::SRT_SOCKOPT::*;
        match self {
            SockOpt::MaxBw => SRTO_MAXBW,
            SockOpt::InputBw => SRTO_INPUTBW,
            SockOpt::OverheadBw => SRTO_OHEADBW,
            SockOpt::SendBuf => SRTO_SNDBUF,
            SockOpt::RecvBuf => SRTO_RCVBUF,
            SockOpt::FlowWindow => SRTO_FC,
            SockOpt::Mss => SRTO_MSS,
            SockOpt::TooLateDrop => SRTO_TLPKTDROP,
            SockOpt::NakReport => SRTO_NAKREPORT,
            SockOpt::LossMaxTtl => SRTO_LOSSMAXTTL,
            SockOpt::PeerIdleTimeout => SRTO_PEERIDLETIMEO,
            SockOpt::StreamId => SRTO_STREAMID,
            SockOpt::KmRefreshRate => SRTO_KMREFRESHRATE,
            SockOpt::KmPreannounce => SRTO_KMPREANNOUNCE,
            SockOpt::Congestion => SRTO_CONGESTION,
            SockOpt::RetransmitAlgo => SRTO_RETRANSMITALGO,
        }
    }

    /// Apply this option's `value` to `sock`, routing each libsrt value type
    /// through its dedicated `ffi` setter.
    fn apply(self, sock: RawSocket, value: &OptValue) -> Result<()> {
        // `SRTO_STREAMID`'s wire format caps the identifier at 512 bytes.
        if let (SockOpt::StreamId, OptValue::Str(s)) = (self, value)
            && s.len() > 512
        {
            return Err(Error::config("stream id must be at most 512 bytes"));
        }
        match value {
            OptValue::I32(v) => ffi::set_flag_i32(sock, self.as_raw(), *v),
            OptValue::I64(v) => ffi::set_flag_i64(sock, self.as_raw(), *v),
            OptValue::Bool(b) => ffi::set_flag_bool(sock, self.as_raw(), *b),
            OptValue::Str(s) => ffi::set_flag_bytes(sock, self.as_raw(), s.as_bytes()),
        }
    }
}

/// A typed value for a [`SockOpt`], mirroring libsrt's option value types.
#[derive(Debug, Clone)]
pub(crate) enum OptValue {
    I32(i32),
    I64(i64),
    Bool(bool),
    Str(String),
}

/// Convert a [`Duration`] to whole milliseconds as a libsrt `int`, saturating.
pub(crate) fn duration_ms(d: Duration) -> i32 {
    i32::try_from(d.as_millis()).unwrap_or(i32::MAX)
}

/// Accumulated, not-yet-applied socket configuration.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    pub transport_type: Option<TransportType>,
    pub latency: Option<Duration>,
    pub recv_latency: Option<Duration>,
    pub peer_latency: Option<Duration>,
    pub passphrase: Option<String>,
    pub key_length: Option<KeyLength>,
    pub payload_size: Option<usize>,
    pub recv_timeout: Option<Duration>,
    pub send_timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    /// Curated options set through the extensible [`SockOpt`] mechanism, applied
    /// in insertion order after the structured options above.
    pub extra: Vec<(SockOpt, OptValue)>,
}

impl Options {
    /// Apply every configured option to a freshly created socket.
    ///
    /// Ordering matters: transport type first (it resets defaults), then key
    /// length before the passphrase so encryption negotiates correctly.
    pub(crate) fn apply(&self, sock: RawSocket) -> Result<()> {
        use srt_io_sys::SRT_SOCKOPT::*;

        if let Some(tt) = self.transport_type {
            ffi::set_flag_i32(sock, SRTO_TRANSTYPE, tt.as_raw())?;
        }
        if let Some(d) = self.latency {
            ffi::set_flag_i32(sock, SRTO_LATENCY, duration_ms(d))?;
        }
        if let Some(d) = self.recv_latency {
            ffi::set_flag_i32(sock, SRTO_RCVLATENCY, duration_ms(d))?;
        }
        if let Some(d) = self.peer_latency {
            ffi::set_flag_i32(sock, SRTO_PEERLATENCY, duration_ms(d))?;
        }
        if let Some(size) = self.payload_size {
            let size =
                i32::try_from(size).map_err(|_| Error::config("payload size exceeds i32::MAX"))?;
            ffi::set_flag_i32(sock, SRTO_PAYLOADSIZE, size)?;
        }
        if let Some(d) = self.recv_timeout {
            ffi::set_flag_i32(sock, SRTO_RCVTIMEO, duration_ms(d))?;
        }
        if let Some(d) = self.send_timeout {
            ffi::set_flag_i32(sock, SRTO_SNDTIMEO, duration_ms(d))?;
        }
        if let Some(d) = self.connect_timeout {
            ffi::set_flag_i32(sock, SRTO_CONNTIMEO, duration_ms(d))?;
        }
        if let Some(kl) = self.key_length {
            ffi::set_flag_i32(sock, SRTO_PBKEYLEN, kl.as_raw())?;
        }
        if let Some(pass) = &self.passphrase {
            if !(10..=79).contains(&pass.len()) {
                return Err(Error::config("passphrase must be between 10 and 79 bytes"));
            }
            ffi::set_flag_bytes(sock, SRTO_PASSPHRASE, pass.as_bytes())?;
        }
        // Curated options routed through the extensible mechanism.
        for (opt, value) in &self.extra {
            opt.apply(sock, value)?;
        }
        Ok(())
    }

    /// Record a curated option to be applied at socket-creation time.
    pub(crate) fn push(&mut self, opt: SockOpt, value: OptValue) {
        self.extra.push((opt, value));
    }
}
