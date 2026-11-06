# srt-rs

A safe Rust wrapper over [libsrt](https://github.com/Haivision/srt), the
reference implementation of **SRT (Secure Reliable Transport)**. It links the
vendored C library statically and exposes an idiomatic, strongly-typed API, so
you get interop-correct SRT without hand-writing FFI.

The workspace has two crates:

- **`srt-sys`** — raw FFI bindings over a vendored, statically linked
  **libsrt v1.5.4** (with mbedTLS for encryption). Generated with `bindgen`,
  built with `cmake`.
- **`srt`** — the safe wrapper you depend on. All `unsafe` is confined to a
  single `ffi` module (the crate is `#![deny(unsafe_code)]` everywhere else);
  every public type reduces libsrt's C surface to ordinary safe Rust.

## Features

- RAII runtime management (`Library`) — reference-counted `srt_startup` /
  `srt_cleanup`.
- Fluent, typed configuration (`Builder`): transport profile, latency,
  encryption (passphrase + AES key length), buffers, bandwidth caps,
  reliability knobs, stream id, congestion controller, and more.
- Blocking `Socket` / `Listener` with `send` / `recv` / `accept`.
- Transport statistics (`Socket::stats`) — RTT, bandwidth, throughput, packet
  and byte counters, buffer levels.
- Optional **`tokio`** feature adding a fully async layer (`connect_async`,
  `bind_async`, `accept_async`, `send_async`, `recv_async`) driven by an
  `srt_epoll` reactor. Off by default — the blocking API pulls in no Tokio.

## Build prerequisites

Building `srt-sys` compiles libsrt from the vendored source, so you need:

- A **C/C++ toolchain** (clang or gcc).
- **cmake**.
- **libclang** (used by `bindgen` to generate the bindings).

libsrt and mbedTLS are **statically linked** — there is no runtime shared-library
dependency once built.

## Adding it as a dependency

This crate is consumed via git or a local path (not yet published to crates.io):

```toml
# Git
[dependencies]
srt = { git = "https://example.com/your/srt-rs.git" }

# Or a local path
[dependencies]
srt = { path = "../srt-rs/srt" }
```

Enable the async layer with the `tokio` feature:

```toml
[dependencies]
srt = { git = "https://example.com/your/srt-rs.git", features = ["tokio"] }
```

## Usage

```rust
use std::time::Duration;
use srt::{Library, TransportType};

fn main() -> srt::Result<()> {
    let lib = Library::init()?;

    // Caller: connect to a listener and send a message.
    let socket = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .connect_timeout(Duration::from_secs(5))
        .connect("127.0.0.1:4201")?;

    socket.send(b"hello over SRT")?;

    let stats = socket.stats(false)?;
    println!("rtt={:.1}ms sent={} pkts", stats.rtt_ms, stats.packets_sent);
    Ok(())
}
```

Runnable examples live in [`srt/examples/`](srt/examples):

```sh
# Blocking
cargo run -p srt --example listener
cargo run -p srt --example caller

# Async (Tokio feature)
cargo run -p srt --features tokio --example async_listener
cargo run -p srt --features tokio --example async_caller
```

## Interoperability

This wrapper is verified interoperable with the reference libsrt implementation
(e.g. `srt-live-transmit`); see [`tools/interop/INTEROP.md`](tools/interop/INTEROP.md).

## License

Apache-2.0.
