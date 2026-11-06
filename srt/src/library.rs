//! RAII initialisation of the libsrt runtime.
//!
//! [`Library`] is a reference-counted guard. The first live handle calls
//! `srt_startup()`; the last one dropped calls `srt_cleanup()`. Cloning bumps
//! the count. Because [`Socket`](crate::Socket), [`Listener`](crate::Listener)
//! and [`Builder`](crate::Builder) all hold a `Library`, the runtime is
//! guaranteed to stay initialised for as long as any socket exists — sockets
//! cannot be created or used without an active `Library`.

use std::sync::Mutex;

use crate::builder::Builder;
use crate::error::Result;
use crate::ffi;

/// Process-wide count of live [`Library`] handles. Guards the matched
/// `srt_startup` / `srt_cleanup` calls so they are serialised and balanced.
static REFCOUNT: Mutex<usize> = Mutex::new(0);

/// A handle to the initialised libsrt runtime.
///
/// Obtain one with [`Library::init`]. Keep it alive for as long as you use SRT
/// sockets; drop it (and all sockets) to tear the runtime down.
#[derive(Debug)]
pub struct Library {
    // Marker only: the real state lives in `REFCOUNT`. Not constructible
    // outside this module, so every `Library` corresponds to one increment.
    _private: (),
}

impl Library {
    /// Initialise libsrt (idempotent and reference-counted).
    ///
    /// The first successful call runs `srt_startup()`; subsequent calls just
    /// increment the reference count. Returns a guard that keeps the runtime
    /// alive until dropped.
    #[tracing::instrument(level = "debug")]
    pub fn init() -> Result<Library> {
        let mut count = REFCOUNT.lock().expect("srt library refcount poisoned");
        if *count == 0 {
            ffi::startup()?;
            tracing::debug!("libsrt runtime started");
        }
        *count += 1;
        Ok(Library { _private: () })
    }

    /// Start configuring a socket. The returned [`Builder`] borrows nothing and
    /// carries its own `Library` handle, so it (and the sockets it produces)
    /// keep the runtime alive independently of `self`.
    pub fn builder(&self) -> Builder {
        Builder::new(self.clone())
    }
}

impl Clone for Library {
    fn clone(&self) -> Self {
        let mut count = REFCOUNT.lock().expect("srt library refcount poisoned");
        *count += 1;
        Library { _private: () }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        let mut count = REFCOUNT.lock().expect("srt library refcount poisoned");
        *count -= 1;
        if *count == 0 {
            ffi::cleanup();
            tracing::debug!("libsrt runtime stopped");
        }
    }
}
