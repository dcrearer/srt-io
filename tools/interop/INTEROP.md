# SRT Interoperability Test Harness

This directory documents the interop test suite that proves our FFI-bound libsrt
build (the `srt-sys` crate) actually interoperates — both with itself and with
the **reference** Haivision SRT tooling. Interop correctness is the entire
reason this project binds the reference C library instead of a pure-Rust SRT
crate.

All tests operate purely at the raw `srt_sys` FFI level (lots of `unsafe`), so
they are independent of the safe `srt` crate.

## Test layout

| File | What it covers | Default `cargo test`? |
|------|----------------|-----------------------|
| `srt-sys/tests/common/mod.rs`    | Shared FFI helpers (init, sockaddr construction, option setters, payload gen). | n/a (module) |
| `srt-sys/tests/loopback.rs`      | In-process loopback over 127.0.0.1: listener + caller on separate threads, blocking sockets. Plaintext, AES-128, AES-256, plus a wrong-passphrase negative control. | **Yes — runs and passes** |
| `srt-sys/tests/cross_impl.rs`    | Cross-implementation interop against `srt-live-transmit`. Both roles. | No — `#[ignore]` (needs external binary) |

## Versions observed

| Component | Version |
|-----------|---------|
| Our vendored libsrt (`srt-sys`)        | **1.5.4** (static, mbedTLS 3.6.2) |
| Reference `srt-live-transmit` binary   | links libsrt **1.5.6** (`Built with SRT Library version: 1.5.6`) |
| Homebrew `srt` formula                 | 1.5.7 (the shipped `srt-live-transmit` reports the 1.5.6 lib internally) |
| Platform                               | macOS (darwin, arm64) |

So the cross-impl tests exercise a genuine **1.5.4 ⟷ 1.5.6** wire interop.

## Obtaining the reference peer (macOS)

```sh
brew install srt          # installs srt-live-transmit at /opt/homebrew/bin/srt-live-transmit
srt-live-transmit -version
```

If the binary is not on `PATH`, point the tests at it explicitly with the
`SRT_LIVE_TRANSMIT` environment variable (see below).

On Linux you can instead build libsrt's apps in a throwaway dir
(`cmake -DENABLE_APPS=ON …`) and use the resulting `srt-live-transmit`.

## Running the tests

### FFI loopback (always runs)

```sh
cargo test -p srt-sys --test loopback
```

Round-trips a 1200-byte payload between a listener and a caller, each on its own
thread, with blocking (`SRTO_RCVSYN`/`SRTO_SNDSYN = 1`) live-mode sockets.

- `loopback_plaintext` — no encryption.
- `loopback_aes128` — `SRTO_PASSPHRASE` + `SRTO_PBKEYLEN = 16`.
- `loopback_aes256` — `SRTO_PASSPHRASE` + `SRTO_PBKEYLEN = 32`.
- `loopback_wrong_passphrase_rejected` — negative control: a caller with the
  wrong passphrase is rejected (`BADSECRET`), proving encryption is enforced.

### Cross-implementation (requires `srt-live-transmit`)

```sh
# with the binary on PATH:
cargo test -p srt-sys --test cross_impl -- --ignored --nocapture

# or point at a specific binary:
SRT_LIVE_TRANSMIT=/opt/homebrew/bin/srt-live-transmit \
    cargo test -p srt-sys --test cross_impl -- --ignored --nocapture
```

Run serially (`--test-threads=1`) if you want the two roles not to overlap; they
also pass in parallel.

- `cross_our_caller_ref_listener` — **our socket is the caller**, the reference
  tool is the listener:
  `srt-live-transmit 'srt://127.0.0.1:PORT?mode=listener&transtype=live' 'file://con' -autoreconnect:no`.
  Our caller connects and sends a 40-message / 48000-byte stream; the reference
  writes received SRT data to its stdout, which we read back and compare
  byte-for-byte.
- `cross_our_listener_ref_caller` — **our socket is the listener**, the
  reference tool is the caller:
  `srt-live-transmit 'file://con' 'srt://127.0.0.1:PORT?mode=caller&transtype=live'`.
  We feed a 1200-byte payload to the tool's stdin; it sends it over SRT to our
  listener, which accepts and receives it.

## Actual results observed (2026-11-06)

### `cargo test -p srt-sys` (default — loopback + smoke)

```
running 1 test
test tests::startup_and_cleanup ... ok

running 4 tests
test loopback_plaintext ... ok
test loopback_aes128 ... ok
test loopback_aes256 ... ok
test loopback_wrong_passphrase_rejected ... ok
test result: ok. 4 passed; 0 failed; 0 ignored

running 2 tests  (tests/cross_impl.rs)
test cross_our_caller_ref_listener ... ignored, requires external srt-live-transmit
test cross_our_listener_ref_caller ... ignored, requires external srt-live-transmit
```

The wrong-passphrase control prints the expected libsrt rejection log:

```
KMREQ/rcv: (snd) Rx process failure - BADSECRET
interpretSrtHandshake: KMREQ result abnornal - rejecting per enforced encryption
newConnection: connection rejected due to: INTERNAL REJECTION - ERROR:BADSECRET
processConnectRequest: rsp(REJECT): 1010 - Incorrect passphrase
```

### `cargo test -p srt-sys --test cross_impl -- --ignored` (against srt-live-transmit 1.5.6)

```
running 2 tests
test cross_our_caller_ref_listener ... ok   # ROLE A: 48000 bytes verified across 40 messages
test cross_our_listener_ref_caller ... ok   # ROLE B: 1200 payload bytes verified (1456 total incl. tool padding)
test result: ok. 2 passed; 0 failed
```

Reference-tool stderr during Role A confirms a real cross-version handshake:

```
Media path: 'srt://127.0.0.1:PORT?mode=listener&transtype=live&latency=120' --> 'file://con'
connection on listener @... (127.0.0.1:PORT) from peer @... (127.0.0.1:...)
Accepted SRT source connection
SRT source disconnected
```

**Result: our libsrt 1.5.4 interoperates cleanly with the reference 1.5.6
`srt-live-transmit` in both directions, plaintext live mode, payload intact.**

## Protocol-level observations / caveats

1. **Live message API requires a full-size receive buffer.** In live (message)
   mode `srt_recv` rejects a buffer smaller than the negotiated payload size
   with `Operation not supported: Incorrect use of Message API`. The tests use a
   1500-byte receive buffer. This is a libsrt API contract, not a bug.

2. **`srt-live-transmit`'s file source pads to a transmission unit.** When the
   reference tool reads from a *file* medium (`file://con` stdin) and sends over
   SRT, it ships fixed-size transmission-unit blocks and **zero-pads a short
   final read**. So our 1200-byte stdin feed arrives at our listener as a single
   ~1456-byte SRT message whose first 1200 bytes are exactly our payload and
   whose remainder is zeros. This is a framing artifact of the *tool*, not of
   the SRT protocol — SRT itself preserves message boundaries. Role B asserts
   the payload prefix matches exactly and the tail is all zeros. (In the reverse
   direction, Role A, the reference is an SRT *source* and writes exactly the
   bytes it received, so that path is byte-exact with no padding.)

3. **Block-buffered stdout.** `srt-live-transmit`'s `file://con` **output** is
   block-buffered; a single small message can sit in its stdio buffer and be
   lost if the process is SIGKILLed. Role A therefore sends a multi-KB stream
   (forces flushes) and shuts down gracefully with `-autoreconnect:no` so the
   tool flushes stdout and exits on disconnect. If you adapt this harness, do
   not expect tiny payloads to appear on stdout before the process exits.

4. **TSBPD latency drain before close.** Live mode buffers packets for the
   configured latency (default 120 ms). Both the loopback and cross-impl tests
   sleep briefly after the last send before closing the sender, so the receiver
   drains the latency buffer rather than losing the tail.

5. **`file://` scheme is console-only.** `srt-live-transmit` only supports
   `file://con` (stdin/stdout), not arbitrary file paths — hence the pipe-based
   harness rather than a temp-file comparison.
