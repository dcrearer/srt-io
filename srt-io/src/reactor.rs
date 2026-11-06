//! The async readiness reactor (only compiled with the `tokio` feature).
//!
//! A [`Reactor`] owns one SRT epoll id and one dedicated background thread. The
//! thread runs [`srt_epoll_uwait`](crate::ffi::epoll_uwait) in a loop, turning
//! libsrt readiness into task wakeups. Async sockets register with the reactor,
//! arm interest in readability/writability on demand, and are woken when the
//! corresponding event fires.
//!
//! # Readiness model
//!
//! Interest is **level-triggered** but subscribed one-shot per direction: a
//! task arms `IN`/`OUT` only while it is actually waiting, and the reactor
//! removes that bit the moment it fires. This has two consequences that make
//! the bridge race-free *and* cheap:
//!
//! - No busy loop: once an event fires its interest bit is cleared, so
//!   `uwait` does not keep reporting the same persistent readiness.
//! - No lost wakeups: a readiness flag (`readable`/`writable`) is latched when
//!   the event fires and only cleared by the task when the operation reports
//!   would-block. Because interest is re-armed *after* that drain, and
//!   level-triggered epoll reports the *current* state, any data that arrived
//!   in the gap is picked up on the next `uwait`.
//!
//! `ERR` is bundled with any active interest so a broken connection always
//! wakes a waiting task; on `ERR` the socket's `err` flag latches and all
//! interest is dropped (again avoiding a busy loop on the persistent error).
//!
//! # Lifecycle
//!
//! The reactor is process-wide and reference-counted through a `Weak`: the
//! first async socket creates it (spawning the thread); when the last async
//! socket drops, the `Arc` count hits zero and [`Reactor::drop`] stops the
//! thread (stop flag + condvar wakeup, bounded `uwait` timeout) and releases
//! the epoll id. No thread is leaked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::Result;
use crate::ffi::{self, RawSocket};

/// How long a single `uwait` blocks before the loop re-checks the stop flag and
/// notices newly registered sockets. Bounds both shutdown latency and the delay
/// before a freshly registered socket is first polled.
const UWAIT_TIMEOUT: i64 = 20;

/// Size of the event batch handed to `srt_epoll_uwait` per iteration.
const EVENT_BATCH: usize = 64;

/// Per-socket readiness bookkeeping, all guarded by [`Shared::state`].
#[derive(Default)]
struct SockState {
    /// Interest currently subscribed with the epoll (`0` = not subscribed).
    mask: i32,
    /// Whether the socket is currently added to the epoll at all.
    subscribed: bool,
    /// Latched readability; cleared by the task on would-block.
    readable: bool,
    /// Latched writability; cleared by the task on would-block.
    writable: bool,
    /// Latched error/broken state; never cleared (socket is finished).
    err: bool,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
}

/// State shared between the reactor handle and its background thread.
struct Shared {
    eid: std::os::raw::c_int,
    stop: AtomicBool,
    state: Mutex<HashMap<RawSocket, SockState>>,
    /// Signals the thread that a socket was registered or that it should stop,
    /// so it does not sleep while there is work to pick up.
    condvar: Condvar,
}

impl Shared {
    /// Recompute and apply a socket's epoll subscription from its wanted
    /// interest. Adds/updates/removes the usock as needed. Must be called with
    /// `state` locked and the entry present.
    ///
    /// Returns `true` when interest was (re)subscribed — i.e. the socket was
    /// just added to, or had a new direction enabled on, the epoll. The caller
    /// uses that to force one immediate re-attempt of the I/O operation:
    /// libsrt's epoll only latches readiness that occurs *after* a socket is
    /// subscribed, so any readiness already present at subscription time would
    /// otherwise be lost. Re-attempting the syscall once picks it up; every
    /// later event is delivered normally by the reactor.
    fn apply_interest(
        &self,
        sock: RawSocket,
        st: &mut SockState,
        want_read: bool,
        want_write: bool,
    ) -> bool {
        let mut mask = 0;
        if want_read {
            mask |= ffi::EPOLL_IN;
        }
        if want_write {
            mask |= ffi::EPOLL_OUT;
        }
        if mask != 0 {
            mask |= ffi::EPOLL_ERR;
        }
        if mask == st.mask {
            return false;
        }
        // A newly enabled direction (adding IN/OUT) may race with readiness
        // that already happened; a pure narrowing (removing a bit) cannot.
        let newly_armed = (mask & !st.mask) & (ffi::EPOLL_IN | ffi::EPOLL_OUT) != 0;
        let result = if mask == 0 {
            let r = ffi::epoll_remove_usock(self.eid, sock);
            st.subscribed = false;
            r
        } else if !st.subscribed {
            let r = ffi::epoll_add_usock(self.eid, sock, mask);
            if r.is_ok() {
                st.subscribed = true;
            }
            r
        } else {
            ffi::epoll_update_usock(self.eid, sock, mask)
        };
        if let Err(e) = &result {
            tracing::warn!(sock, error = %e, "epoll interest update failed");
            // Leave `mask` unchanged so the next poll retries the subscription,
            // and force a re-attempt rather than parking against a dead socket.
            return true;
        }
        st.mask = mask;
        newly_armed
    }
}

/// The async readiness reactor. Cloneable handles share one thread via `Arc`.
pub(crate) struct Reactor {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Reactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reactor")
            .field("eid", &self.shared.eid)
            .finish_non_exhaustive()
    }
}

/// Process-wide reactor, held weakly so it is torn down once no async socket
/// references it.
static GLOBAL: Mutex<Weak<Reactor>> = Mutex::new(Weak::new());

impl Reactor {
    /// Return the shared process-wide reactor, creating (and starting) it if no
    /// async socket currently keeps one alive.
    pub(crate) fn global() -> Result<Arc<Reactor>> {
        let mut guard = GLOBAL.lock().expect("reactor global lock poisoned");
        if let Some(existing) = guard.upgrade() {
            return Ok(existing);
        }
        let reactor = Arc::new(Reactor::new()?);
        *guard = Arc::downgrade(&reactor);
        Ok(reactor)
    }

    fn new() -> Result<Self> {
        let eid = ffi::epoll_create()?;
        let shared = Arc::new(Shared {
            eid,
            stop: AtomicBool::new(false),
            state: Mutex::new(HashMap::new()),
            condvar: Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("srt-reactor".to_owned())
            .spawn(move || run(&thread_shared))
            .expect("spawn srt reactor thread");
        tracing::debug!(eid, "srt reactor started");
        Ok(Reactor {
            shared,
            thread: Some(thread),
        })
    }

    /// Register a socket with the reactor (initially with no interest). Safe to
    /// call once per socket.
    pub(crate) fn register(&self, sock: RawSocket) {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        state.entry(sock).or_default();
        drop(state);
        // Nudge the thread in case it was idle-waiting with no sockets.
        self.shared.condvar.notify_all();
    }

    /// Remove a socket from the reactor and the epoll. Called on socket drop.
    pub(crate) fn deregister(&self, sock: RawSocket) {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        if let Some(st) = state.remove(&sock)
            && st.subscribed
            && let Err(e) = ffi::epoll_remove_usock(self.shared.eid, sock)
        {
            tracing::warn!(sock, error = %e, "epoll deregister failed");
        }
    }

    /// Poll for readability. Returns `Ready` once the socket is readable (or
    /// errored); otherwise arms read interest and returns `Pending`.
    pub(crate) fn poll_readable(&self, sock: RawSocket, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        let st = state.entry(sock).or_default();
        if st.readable || st.err {
            return Poll::Ready(());
        }
        st.read_waker = Some(cx.waker().clone());
        let want_write = st.writable_interest();
        let newly_armed = self.shared.apply_interest(sock, st, true, want_write);
        // Re-check after arming to close any race with the reactor thread, and
        // force one re-attempt if we just (re)subscribed (see `apply_interest`).
        if st.readable || st.err || newly_armed {
            Poll::Ready(())
        } else {
            drop(state);
            // Wake the reactor so freshly armed interest is polled at once,
            // rather than after the idle backstop timeout.
            self.shared.condvar.notify_one();
            Poll::Pending
        }
    }

    /// Poll for writability. Returns `Ready` once the socket is writable (or
    /// errored); otherwise arms write interest and returns `Pending`.
    pub(crate) fn poll_writable(&self, sock: RawSocket, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        let st = state.entry(sock).or_default();
        if st.writable || st.err {
            return Poll::Ready(());
        }
        st.write_waker = Some(cx.waker().clone());
        let want_read = st.readable_interest();
        let newly_armed = self.shared.apply_interest(sock, st, want_read, true);
        if st.writable || st.err || newly_armed {
            Poll::Ready(())
        } else {
            drop(state);
            self.shared.condvar.notify_one();
            Poll::Pending
        }
    }

    /// Clear the latched readability after an operation reported would-block, so
    /// the next poll re-arms interest.
    pub(crate) fn clear_readable(&self, sock: RawSocket) {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        if let Some(st) = state.get_mut(&sock) {
            st.readable = false;
        }
    }

    /// Clear the latched writability after an operation reported would-block.
    pub(crate) fn clear_writable(&self, sock: RawSocket) {
        let mut state = self.shared.state.lock().expect("reactor state poisoned");
        if let Some(st) = state.get_mut(&sock) {
            st.writable = false;
        }
    }
}

impl SockState {
    /// Whether a reader is currently parked (read interest should stay armed).
    fn readable_interest(&self) -> bool {
        self.mask & ffi::EPOLL_IN != 0
    }

    /// Whether a writer is currently parked (write interest should stay armed).
    fn writable_interest(&self) -> bool {
        self.mask & ffi::EPOLL_OUT != 0
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.condvar.notify_all();
        if let Some(handle) = self.thread.take()
            && handle.join().is_err()
        {
            tracing::error!("srt reactor thread panicked");
        }
        ffi::epoll_release(self.shared.eid);
        tracing::debug!(eid = self.shared.eid, "srt reactor stopped");
    }
}

/// The reactor thread body: wait for events and wake the matching tasks.
fn run(shared: &Arc<Shared>) {
    let mut events = vec![ffi::EpollEvent { fd: 0, events: 0 }; EVENT_BATCH];
    while !shared.stop.load(Ordering::SeqCst) {
        // If nothing is subscribed, sleep on the condvar rather than spinning
        // `uwait` on an empty set (which libsrt reports as an error).
        {
            let state = shared.state.lock().expect("reactor state poisoned");
            let idle = state.values().all(|st| !st.subscribed);
            if idle {
                let _unused = shared
                    .condvar
                    .wait_timeout(state, Duration::from_millis(200))
                    .expect("reactor state poisoned");
                continue;
            }
        }

        let n = match ffi::epoll_uwait(shared.eid, &mut events, UWAIT_TIMEOUT) {
            Ok(n) => n,
            Err(e) => {
                // Transient (e.g. the set briefly emptied). Back off a touch.
                tracing::trace!(error = %e, "epoll_uwait returned error");
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        };
        if n == 0 {
            continue;
        }

        let mut to_wake: Vec<Waker> = Vec::new();
        {
            let mut state = shared.state.lock().expect("reactor state poisoned");
            for ev in &events[..n] {
                let sock = ev.fd;
                let Some(st) = state.get_mut(&sock) else {
                    continue;
                };
                let flags = ev.events;
                let mut want_read = st.readable_interest();
                let mut want_write = st.writable_interest();
                if flags & ffi::EPOLL_ERR != 0 {
                    st.err = true;
                    want_read = false;
                    want_write = false;
                    if let Some(w) = st.read_waker.take() {
                        to_wake.push(w);
                    }
                    if let Some(w) = st.write_waker.take() {
                        to_wake.push(w);
                    }
                }
                if flags & ffi::EPOLL_IN != 0 {
                    st.readable = true;
                    want_read = false;
                    if let Some(w) = st.read_waker.take() {
                        to_wake.push(w);
                    }
                }
                if flags & ffi::EPOLL_OUT != 0 {
                    st.writable = true;
                    want_write = false;
                    if let Some(w) = st.write_waker.take() {
                        to_wake.push(w);
                    }
                }
                shared.apply_interest(sock, st, want_read, want_write);
            }
        }
        for waker in to_wake {
            waker.wake();
        }
    }
}
