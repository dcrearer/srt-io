//! Asynchronous SRT caller (requires the `tokio` feature). Pair it with the
//! `async_listener` example.
//!
//! ```text
//! cargo run -p srt --features tokio --example async_listener
//! cargo run -p srt --features tokio --example async_caller
//! ```

use std::time::Duration;

use srt_io::{Library, Result, TransportType};

const DEFAULT_ADDR: &str = "127.0.0.1:4202";

#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ADDR.into());

    let lib = Library::init()?;

    let socket = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .connect_timeout(Duration::from_secs(5))
        .stream_id("example-async-caller")
        .connect_async(&addr)
        .await?;
    println!("connected to {}", socket.peer_addr()?);

    let mut buf = [0u8; 1500];
    for i in 0..5 {
        let msg = format!("message {i}");
        socket.send_async(msg.as_bytes()).await?;
        let n = socket.recv_async(&mut buf).await?;
        assert_eq!(&buf[..n], msg.as_bytes(), "echo mismatch");
        println!("echoed back: {}", String::from_utf8_lossy(&buf[..n]));
    }

    let stats = socket.stats(false)?;
    println!(
        "stats: rtt={:.1}ms send_rate={:.3}Mbps pkts_sent={}",
        stats.rtt_ms, stats.send_rate_mbps, stats.packets_sent
    );

    Ok(())
}
