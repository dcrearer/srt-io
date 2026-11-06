//! Cross-implementation interop tests: OUR vendored libsrt (v1.5.4, via the
//! `srt_io_sys` FFI) exchanging data with the REFERENCE Haivision tooling
//! (`srt-live-transmit`, which links its own libsrt — Homebrew ships v1.5.6).
//!
//! Both roles are covered:
//!   * `cross_our_caller_ref_listener` — our socket CALLS a reference listener.
//!   * `cross_our_listener_ref_caller` — our socket LISTENS, a reference caller
//!     connects.
//!
//! We bridge the reference side to a pipe with `file://con` (stdin/stdout),
//! which is loss-free and deterministic — better than a UDP bridge for an
//! integrity assertion.
//!
//! These are `#[ignore]`d because they need the external `srt-live-transmit`
//! binary. To run them:
//!
//! ```text
//! # Obtain the reference peer (macOS):
//! brew install srt            # provides srt-live-transmit
//!
//! # Then, from the workspace root:
//! cargo test -p srt-io-sys --test cross_impl -- --ignored --nocapture
//!
//! # If the binary is not on PATH, point at it explicitly:
//! SRT_LIVE_TRANSMIT=/opt/homebrew/bin/srt-live-transmit \
//!     cargo test -p srt-io-sys --test cross_impl -- --ignored --nocapture
//! ```

mod common;

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use srt_io_sys::{
    SRT_INVALID_SOCK, srt_accept, srt_bind, srt_close, srt_connect, srt_create_socket, srt_listen,
    srt_recv, srt_send,
};

/// Locate the reference `srt-live-transmit` binary.
fn ref_binary() -> String {
    std::env::var("SRT_LIVE_TRANSMIT").unwrap_or_else(|_| "srt-live-transmit".to_string())
}

/// Pick a semi-random high port to reduce collisions between runs/roles.
fn pick_port(salt: u16) -> u16 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    20_000 + ((nanos as u16).wrapping_add(salt) % 20_000)
}

const PAYLOAD_LEN: usize = 1200;

/// ROLE A: our side is the CALLER, the reference tool is the LISTENER.
///
/// `srt-live-transmit 'srt://127.0.0.1:PORT?mode=listener' 'file://con'`
/// receives from SRT and writes the bytes to its stdout, which we read back and
/// compare against what our caller sent.
#[test]
#[ignore = "requires external srt-live-transmit (reference libsrt); see module docs"]
fn cross_our_caller_ref_listener() {
    init();
    // Send a multi-message stream. A single small message gets stuck in
    // srt-live-transmit's block-buffered stdout and is lost when the process is
    // killed; a larger stream forces flushes and the graceful shutdown below
    // flushes the tail.
    const MSG: usize = PAYLOAD_LEN;
    const N_MSGS: usize = 40;
    let stream: Vec<u8> = make_payload(MSG * N_MSGS);
    let port = pick_port(1);

    let input = format!("srt://127.0.0.1:{port}?mode=listener&transtype=live&latency=120");
    let mut child = Command::new(ref_binary())
        .arg(&input)
        .arg("file://con")
        .arg("-loglevel:note")
        .arg("-autoreconnect:no") // exit (and flush stdout) when our caller disconnects
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn srt-live-transmit (is it installed / on PATH?)");

    // Collect the reference tool's stdout + stderr in the background.
    let out_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let reader = spawn_reader(child.stdout.take().unwrap(), out_buf.clone());
    let err_reader = spawn_reader(child.stderr.take().unwrap(), err_buf.clone());

    // Wait for the listener to come up, then connect as a fresh caller.
    let caller = connect_with_retry(port, Duration::from_secs(10));

    // Send the stream one SRT message at a time (blocking send).
    for i in 0..N_MSGS {
        let chunk = &stream[i * MSG..(i + 1) * MSG];
        let sent = unsafe {
            srt_send(
                caller,
                chunk.as_ptr() as *const std::os::raw::c_char,
                chunk.len() as i32,
            )
        };
        assert_eq!(sent, chunk.len() as i32, "srt_send: {}", last_error());
    }

    // Let the TSBPD latency buffer drain before we tear the connection down.
    thread::sleep(Duration::from_millis(600));
    unsafe { srt_close(caller) };

    // With autoreconnect off the tool exits on disconnect, flushing stdout; the
    // reader then hits EOF. Guard against a hang with a kill fallback.
    wait_or_kill(&mut child, Duration::from_secs(8));
    let _ = reader.join();
    let _ = err_reader.join();
    let got = out_buf.lock().unwrap().clone();

    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).into_owned();
    eprintln!("--- srt-live-transmit stderr (role A) ---\n{stderr}\n---");

    assert!(
        !got.is_empty(),
        "reference listener produced no output (our caller -> ref listener)"
    );
    // The SRT source writes exactly the bytes it received, in order, with no
    // padding — so the concatenated stdout must equal our sent stream exactly.
    assert_eq!(
        got.len(),
        stream.len(),
        "byte count mismatch: got {} expected {}",
        got.len(),
        stream.len()
    );
    assert_eq!(
        got, stream,
        "payload corrupted across implementations (our caller -> ref listener)"
    );
    eprintln!(
        "ROLE A (our caller -> ref listener): {} bytes verified across {} messages",
        got.len(),
        N_MSGS
    );
}

/// ROLE B: our side is the LISTENER, the reference tool is the CALLER.
///
/// `srt-live-transmit 'file://con' 'srt://127.0.0.1:PORT?mode=caller'`
/// reads our payload from its stdin and sends it over SRT to our listener,
/// which accepts and receives it.
#[test]
#[ignore = "requires external srt-live-transmit (reference libsrt); see module docs"]
fn cross_our_listener_ref_caller() {
    init();
    let payload = make_payload(PAYLOAD_LEN);

    // Our listener on an ephemeral port.
    let listener = unsafe { srt_create_socket() };
    assert_ne!(
        listener,
        SRT_INVALID_SOCK,
        "create listener: {}",
        last_error()
    );
    set_blocking(listener);
    set_live(listener);
    let bind_addr = sockaddr_in_v4([127, 0, 0, 1], 0);
    assert_eq!(
        unsafe { srt_bind(listener, as_sockaddr_ptr(&bind_addr), sockaddr_len()) },
        0,
        "srt_bind: {}",
        last_error()
    );
    let port = local_port(listener);
    assert_eq!(
        unsafe { srt_listen(listener, 1) },
        0,
        "srt_listen: {}",
        last_error()
    );

    // Spawn the reference caller once we are listening.
    let output = format!("srt://127.0.0.1:{port}?mode=caller&transtype=live");
    let mut child = Command::new(ref_binary())
        .arg("file://con")
        .arg(&output)
        .arg("-loglevel:error")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn srt-live-transmit (is it installed / on PATH?)");

    // Accept the reference connection on a worker thread so a stuck handshake
    // cannot wedge the whole test harness indefinitely.
    let accept_handle = thread::spawn(move || {
        let accepted = unsafe { srt_accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_ne!(accepted, SRT_INVALID_SOCK, "srt_accept: {}", last_error());
        let mut buf = vec![0u8; 1500];
        let n = unsafe {
            srt_recv(
                accepted,
                buf.as_mut_ptr() as *mut std::os::raw::c_char,
                buf.len() as i32,
            )
        };
        assert!(n > 0, "srt_recv returned {n}: {}", last_error());
        buf.truncate(n as usize);
        unsafe {
            srt_close(accepted);
            srt_close(listener);
        }
        buf
    });

    // Give the caller a moment to connect, then feed it the payload via stdin.
    thread::sleep(Duration::from_millis(500));
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&payload).expect("write to ref stdin");
        stdin.flush().ok();
        // Dropping stdin closes the pipe -> EOF, so the tool flushes the chunk.
    }

    let received = accept_handle.join().expect("accept thread panicked");

    let _ = child.kill();
    let _ = child.wait();

    // NOTE: srt-live-transmit's file-source transmits fixed-size transmission
    // units (the SRT payload size, 1316 bytes), zero-padding a short final
    // read. So our 1200-byte stdin feed arrives as a 1316-byte SRT message
    // whose first 1200 bytes are our payload. Integrity is proven by that
    // prefix matching exactly; the trailing padding is a framing artifact of
    // the reference *tool*, not of the SRT protocol.
    assert!(
        received.len() >= payload.len(),
        "received fewer bytes ({}) than sent ({})",
        received.len(),
        payload.len()
    );
    assert_eq!(
        &received[..payload.len()],
        &payload[..],
        "payload corrupted across implementations (ref caller -> our listener)"
    );
    assert!(
        received[payload.len()..].iter().all(|&b| b == 0),
        "unexpected non-zero trailing bytes beyond the sent payload"
    );
    eprintln!(
        "ROLE B (ref caller -> our listener): {} payload bytes verified ({} total incl. tool padding)",
        payload.len(),
        received.len()
    );
}

// --- helpers -------------------------------------------------------------

/// Create a fresh caller socket and retry `srt_connect` until the reference
/// listener is up (or we give up after `timeout`).
fn connect_with_retry(port: u16, timeout: Duration) -> srt_io_sys::SRTSOCKET {
    let addr = sockaddr_in_v4([127, 0, 0, 1], port);
    let deadline = Instant::now() + timeout;
    loop {
        let caller = unsafe { srt_create_socket() };
        assert_ne!(caller, SRT_INVALID_SOCK, "create caller: {}", last_error());
        set_blocking(caller);
        set_live(caller);
        let rc = unsafe { srt_connect(caller, as_sockaddr_ptr(&addr), sockaddr_len()) };
        if rc == 0 {
            return caller;
        }
        let last = last_error();
        unsafe { srt_close(caller) };
        if Instant::now() >= deadline {
            panic!("could not connect to reference listener on :{port}: {last}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Wait for a child to exit, killing it if it outlives `timeout`.
fn wait_or_kill(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
}

/// Drain a child stdout pipe into a shared buffer until EOF.
fn spawn_reader<R: Read + Send + 'static>(
    mut r: R,
    buf: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    })
}
