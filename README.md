# srt-io

Safe, idiomatic Rust bindings to [libsrt](https://github.com/Haivision/srt) —
the reference implementation of **SRT (Secure Reliable Transport)**, the
low-latency, encrypted, ARQ-based transport protocol used across live video
and streaming.

`srt-io` links the vendored C library **statically** and exposes a
strongly-typed, memory-safe API, so you get interop-correct SRT without
hand-writing FFI or shipping a shared library.

> **Unofficial project.** These are independent, community-maintained bindings.
> They are **not** affiliated with, endorsed by, or supported by Haivision or
> the SRT Alliance. "SRT" and "libsrt" belong to their respective owners.

## Why bindings and not a pure-Rust SRT?

Interop correctness. A pure-Rust reimplementation has to re-derive the protocol
and keep it bit-compatible with the reference across versions — which is exactly
where existing pure-Rust crates fall down (their conformance tests against the
reference implementation do not pass). By binding the reference `libsrt`, this
crate interoperates with real SRT endpoints by construction. It is verified
byte-exact against `srt-live-transmit` across libsrt versions (see
[Interoperability](#interoperability)).

## Crates

| Crate | Role |
|-------|------|
| [`srt-io`](srt-io)         | The safe wrapper you depend on. `#![deny(unsafe_code)]`; all `unsafe` is confined to one internal `ffi` module. Typed errors (`thiserror`), structured logging (`tracing`). |
| [`srt-io-sys`](srt-io-sys) | Raw `bindgen` FFI over a vendored, statically built **libsrt v1.5.4** + **mbedTLS v3.6.2**. You usually do not depend on this directly. |

## Features

- **RAII runtime** — `Library` reference-counts `srt_startup` / `srt_cleanup`;
  the runtime is guaranteed live while any socket exists.
- **Caller and listener modes** — connect out (`Builder::connect`) or bind and
  accept (`Builder::bind` → `listen` → `accept`). SRT connections are
  bidirectional, so either end can send and receive.
- **Encryption** — AES-128/192/256 with a passphrase (`SRTO_PASSPHRASE` /
  `SRTO_PBKEYLEN`), plus key-material refresh controls.
- **Fluent, typed configuration** (`Builder`) — transport profile, latency,
  bandwidth caps, send/receive buffers, flow-control window, MSS, too-late-drop,
  NAK reports, loss TTL, peer-idle timeout, stream id, congestion controller,
  retransmit algorithm — over an **extensible** option mechanism (see
  [Socket options](#socket-options)).
- **Transport statistics** (`Socket::stats`) — RTT, bandwidth, send/recv rates,
  packet and byte counters, loss/retransmit/drop counts, buffer levels.
- **Optional async layer** (`tokio` feature) — `connect_async`, `bind_async`,
  `accept_async`, `send_async`, `recv_async` on the *same* types, driven by an
  `srt_epoll` reactor. Off by default; the blocking API pulls in no Tokio.

### Not (yet) supported

- **Rendezvous mode** (simultaneous mutual connect for NAT traversal).
- **Connection bonding / socket groups** (main+backup, broadcast).
- **Packet filtering / FEC** configuration.

These are additive: the option layer is designed so they can be added without
breaking changes. Contributions/requests welcome.

## Build prerequisites

`srt-io-sys` compiles libsrt and mbedTLS **from source** at build time, so every
machine that builds a project depending on `srt-io` needs:

- A **C/C++ toolchain** (clang or gcc).
- **cmake** (3.5+; works with cmake 4.x).
- **libclang** (used by `bindgen`). On macOS it ships with the Xcode Command
  Line Tools; on Debian/Ubuntu install `clang libclang-dev`.

libsrt and mbedTLS are **statically linked**, so the resulting binary has no
runtime shared-library dependency. There is no system libsrt involved.

## Adding it as a dependency

```toml
# Local path
[dependencies]
srt-io = { path = "../srt-io/srt-io" }

# Or git (cargo fetches the vendored libsrt/mbedTLS submodules automatically)
[dependencies]
srt-io = { git = "https://github.com/<you>/srt-io.git" }
```

Enable the async layer with the `tokio` feature:

```toml
[dependencies]
srt-io = { git = "https://github.com/<you>/srt-io.git", features = ["tokio"] }
```

## Quick start

### Listener (receiver)

```rust,no_run
use std::time::Duration;
use srt_io::{Library, TransportType};

fn main() -> srt_io::Result<()> {
    let lib = Library::init()?;

    let listener = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .bind("0.0.0.0:4201")?;
    listener.listen(1)?;

    let (conn, peer) = listener.accept()?;   // blocks until a caller connects
    println!("accepted {peer:?}");

    let mut buf = [0u8; 1500];
    let n = conn.recv(&mut buf)?;
    println!("got {n} bytes");
    Ok(())
}
```

### Caller (sender), encrypted

```rust,no_run
use std::time::Duration;
use srt_io::{KeyLength, Library, TransportType};

fn main() -> srt_io::Result<()> {
    let lib = Library::init()?;

    let socket = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .key_length(KeyLength::Aes256)
        .passphrase("a-shared-passphrase")
        .connect_timeout(Duration::from_secs(5))
        .connect("127.0.0.1:4201")?;

    socket.send(b"hello over SRT")?;

    let s = socket.stats(false)?;
    println!("rtt={:.1}ms sent={} pkts", s.rtt_ms, s.packets_sent);
    Ok(())
}
```

### Async (requires `features = ["tokio"]`)

```rust,ignore
use std::time::Duration;
use srt_io::{Library, TransportType};

#[tokio::main]
async fn main() -> srt_io::Result<()> {
    let lib = Library::init()?;
    let socket = lib
        .builder()
        .transport_type(TransportType::Live)
        .latency(Duration::from_millis(120))
        .connect_async("127.0.0.1:4201")
        .await?;
    socket.send_async(b"hello").await?;
    Ok(())
}
```

## Socket options

Common options have named, typed, fluent setters on `Builder`
(`latency`, `max_bw`, `stream_id`, `too_late_drop`, `congestion`, …). Under the
hood they route through a single extensible mechanism keyed by a
`#[non_exhaustive]` `SockOpt` enum, so wrapping a new libsrt option is a
one-line, non-breaking addition — a new enum variant plus a one-line setter.
See the [crate docs](https://docs.rs/srt-io) for the full list.

## Statistics

`Socket::stats(clear)` returns a safe snapshot of libsrt's performance monitor:
RTT, estimated bandwidth, send/receive rates, cumulative packet/byte counters,
loss/retransmit/drop counts, and live buffer levels. Read it while the
connection is up (a broken socket can no longer report stats).

## Examples

Runnable examples live in [`srt-io/examples/`](srt-io/examples):

```sh
# Blocking (start the listener first, then the caller)
cargo run -p srt-io --example listener
cargo run -p srt-io --example caller

# Async (Tokio feature)
cargo run -p srt-io --features tokio --example async_listener
cargo run -p srt-io --features tokio --example async_caller
```

A small reader/writer CLI that exercises the library end to end lives in the
sibling [`srt-demo`](../srt-demo) crate.

## Interoperability

Verified byte-exact against the reference implementation (`srt-live-transmit`),
in both caller and listener roles and across libsrt versions (our vendored
1.5.4 ↔ a system 1.5.6). See
[`tools/interop/INTEROP.md`](tools/interop/INTEROP.md) for the test matrix and
results.

## Versioning

Vendored and pinned: **libsrt v1.5.4**, **mbedTLS v3.6.2**. Bumping either is a
submodule update plus a rebuild.

## License

The Rust code in this repository is licensed under **Apache-2.0**.

The published crate bundles vendored C sources: **libsrt** (MPL-2.0) and
**mbedTLS** (Apache-2.0). Their license texts are included under
`srt-io-sys/vendor/`. By depending on `srt-io` you also redistribute those
components under their respective licenses.
