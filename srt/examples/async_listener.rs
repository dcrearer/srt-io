//! Asynchronous SRT listener (requires the `tokio` feature).
//!
//! ```text
//! cargo run -p srt --features tokio --example async_listener
//! cargo run -p srt --features tokio --example async_caller
//! ```
//!
//! Every socket operation here is `_async`: nothing blocks the Tokio runtime.

use std::time::Duration;

use srt::{Library, Result, TransportType};

const DEFAULT_ADDR: &str = "127.0.0.1:4202";

#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ADDR.into());

    let lib = Library::init()?;

    let listener = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .bind_async(&addr)
        .await?;
    listener.listen(1)?;
    println!("listening on {}", listener.local_addr()?);

    let (conn, peer) = listener.accept_async().await?;
    println!("accepted connection from {peer:?}");

    let mut buf = [0u8; 1500];
    loop {
        match conn.recv_async(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                println!("recv {n} bytes: {}", String::from_utf8_lossy(&buf[..n]));
                conn.send_async(&buf[..n]).await?;
            }
            Err(e) => {
                eprintln!("recv ended: {e}");
                break;
            }
        }
    }

    Ok(())
}
