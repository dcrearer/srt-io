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
/// defaults when it changes; [`Options::apply`] handles that ordering.
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
            TransportType::Live => srt_sys::SRT_TRANSTYPE::SRTT_LIVE as i32,
            TransportType::File => srt_sys::SRT_TRANSTYPE::SRTT_FILE as i32,
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

/// Convert a [`Duration`] to whole milliseconds as a libsrt `int`, saturating.
fn duration_ms(d: Duration) -> i32 {
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
}

impl Options {
    /// Apply every configured option to a freshly created socket.
    ///
    /// Ordering matters: transport type first (it resets defaults), then key
    /// length before the passphrase so encryption negotiates correctly.
    pub(crate) fn apply(&self, sock: RawSocket) -> Result<()> {
        use srt_sys::SRT_SOCKOPT::*;

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
        Ok(())
    }
}
