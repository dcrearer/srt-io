//! The crate's typed, public error.
//!
//! [`Error`] is the single error type crossing this crate's API boundary. It is
//! built with `thiserror` (the project standard) and its messages are composed
//! from libsrt's own diagnostics: `srt_getlasterror()` / `srt_getlasterror_str()`
//! for the code and human-readable text, and `srt_getrejectreason()` for the
//! connection reject reason on handshake failures.

use crate::ffi;

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the safe SRT API.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `srt_startup()` failed; the libsrt runtime is unavailable.
    #[error("libsrt startup failed: {message}")]
    Startup {
        /// libsrt's diagnostic text.
        message: String,
    },

    /// A libsrt API call failed. `context` names the operation; `code` and
    /// `message` come from `srt_getlasterror()` / `srt_getlasterror_str()`.
    #[error("{context} failed: {message} (srt error {code})")]
    Api {
        /// The operation that failed (e.g. `"bind"`, `"recv"`).
        context: &'static str,
        /// libsrt error code.
        code: i32,
        /// libsrt diagnostic text.
        message: String,
    },

    /// A connection handshake was rejected. Carries both the last-error detail
    /// and the reject reason from `srt_getrejectreason()`.
    #[error(
        "{context} rejected: {message} (srt error {code}); reject reason: {reject_reason} (code {reject_code})"
    )]
    ConnectionRejected {
        /// The operation that was rejected (e.g. `"connect"`).
        context: &'static str,
        /// libsrt error code.
        code: i32,
        /// libsrt diagnostic text.
        message: String,
        /// Numeric reject reason.
        reject_code: i32,
        /// Human-readable reject reason.
        reject_reason: String,
    },

    /// A socket address could not be understood (resolution yielded nothing, or
    /// libsrt returned an unknown address family).
    #[error("invalid address: {0}")]
    Address(String),

    /// A configuration value was invalid before it reached libsrt.
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl Error {
    pub(crate) fn startup(message: String) -> Self {
        Error::Startup { message }
    }

    pub(crate) fn address(message: impl Into<String>) -> Self {
        Error::Address(message.into())
    }

    pub(crate) fn config(message: impl Into<String>) -> Self {
        Error::Config(message.into())
    }

    /// Build an [`Error::Api`] from libsrt's last recorded error.
    pub(crate) fn from_last(context: &'static str) -> Self {
        let (code, message) = ffi::last_error();
        Error::Api {
            context,
            code,
            message,
        }
    }

    /// Build an [`Error::ConnectionRejected`] from a socket's reject reason
    /// alone, without consulting the per-thread last-error slot.
    ///
    /// Used by the async connect path: a non-blocking handshake fails on
    /// libsrt's internal worker threads, so `srt_getlasterror()` on the caller
    /// thread is not meaningful; the reject reason attached to the socket is.
    #[cfg(feature = "tokio")]
    pub(crate) fn from_reject(context: &'static str, sock: ffi::RawSocket) -> Self {
        let (reject_code, reject_reason) = ffi::reject_reason(sock);
        Error::ConnectionRejected {
            context,
            code: srt_sys::SRT_ERROR,
            message: "non-blocking connection failed".to_owned(),
            reject_code,
            reject_reason,
        }
    }

    /// Build an [`Error::ConnectionRejected`] from the last error plus the
    /// socket's reject reason.
    pub(crate) fn from_last_with_reject(context: &'static str, sock: ffi::RawSocket) -> Self {
        let (code, message) = ffi::last_error();
        let (reject_code, reject_reason) = ffi::reject_reason(sock);
        Error::ConnectionRejected {
            context,
            code,
            message,
            reject_code,
            reject_reason,
        }
    }
}
