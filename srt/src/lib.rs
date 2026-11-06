//! Safe Rust wrapper over libsrt (Secure Reliable Transport).
//!
//! Phase 1 fills in the blocking `Socket` / `Listener` / `Builder` surface and
//! the `thiserror` error enum; Phase 2 adds the Tokio async layer behind the
//! same public API. For now this crate only guarantees that `srt-sys` links.

/// Initialize the libsrt runtime. Must be called once before any socket use.
///
/// Returns `true` on success. This thin shim exists so the workspace builds and
/// links end-to-end during Phase 0; it is replaced by RAII init in Phase 1.
pub fn startup() -> bool {
    unsafe { srt_sys::srt_startup() >= 0 }
}

/// Tear down the libsrt runtime.
pub fn cleanup() {
    unsafe {
        srt_sys::srt_cleanup();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_and_initializes() {
        assert!(super::startup());
        super::cleanup();
    }
}
