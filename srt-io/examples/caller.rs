//! Blocking SRT caller: connects to a listener, sends a few messages, and
//! verifies each is echoed back. Pair it with the `listener` example.
//!
//! ```text
//! cargo run -p srt --example listener
//! cargo run -p srt --example caller
//! ```

use std::time::Duration;

use srt_io::{Library, Result, TransportType};

const DEFAULT_ADDR: &str = "127.0.0.1:4201";

fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ADDR.into());

    let lib = Library::init()?;

    let socket = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .connect_timeout(Duration::from_secs(5))
        .stream_id("example-caller")
        .connect(&addr)?;
    println!("connected to {}", socket.peer_addr()?);

    let mut buf = [0u8; 1500];
    for i in 0..5 {
        let msg = format!("message {i}");
        socket.send(msg.as_bytes())?;
        let n = socket.recv(&mut buf)?;
        assert_eq!(&buf[..n], msg.as_bytes(), "echo mismatch");
        println!("echoed back: {}", String::from_utf8_lossy(&buf[..n]));
    }

    let stats = socket.stats(false)?;
    println!(
        "stats: rtt={:.1}ms send_rate={:.3}Mbps pkts_sent={} bytes_sent={}",
        stats.rtt_ms, stats.send_rate_mbps, stats.packets_sent, stats.bytes_sent
    );

    Ok(())
}
