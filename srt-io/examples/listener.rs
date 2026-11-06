//! Blocking SRT listener: binds, accepts one caller, echoes each message back,
//! and prints link statistics on exit.
//!
//! Run alongside the `caller` example:
//!
//! ```text
//! cargo run -p srt --example listener           # binds 127.0.0.1:4201
//! cargo run -p srt --example caller             # connects to it
//! ```
//!
//! Pass an alternate `host:port` as the first argument to either.

use std::time::Duration;

use srt_io::{Library, Result, TransportType};

const DEFAULT_ADDR: &str = "127.0.0.1:4201";

fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ADDR.into());

    // The library handle keeps the libsrt runtime alive for as long as it (and
    // anything it produced) is in scope.
    let lib = Library::init()?;

    let listener = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .receive_timeout(Duration::from_secs(30))
        .bind(&addr)?;
    listener.listen(1)?;
    println!("listening on {}", listener.local_addr()?);

    let (conn, peer) = listener.accept()?;
    println!("accepted connection from {peer:?}");

    let mut buf = [0u8; 1500];
    loop {
        match conn.recv(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                println!("recv {n} bytes: {text}");
                conn.send(&buf[..n])?;
            }
            Err(e) => {
                eprintln!("recv ended: {e}");
                break;
            }
        }
    }

    let stats = conn.stats(false)?;
    println!(
        "stats: rtt={:.1}ms recv_rate={:.3}Mbps pkts_recv={} bytes_recv={}",
        stats.rtt_ms, stats.recv_rate_mbps, stats.packets_received, stats.bytes_received
    );

    Ok(())
}
