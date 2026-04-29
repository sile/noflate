noflate
=======

[![noflate](https://img.shields.io/crates/v/noflate.svg)](https://crates.io/crates/noflate)
[![Documentation](https://docs.rs/noflate/badge.svg)](https://docs.rs/noflate)
[![Actions Status](https://github.com/sile/noflate/workflows/CI/badge.svg)](https://github.com/sile/noflate/actions)
![License](https://img.shields.io/crates/l/noflate)

A zero-dependency DEFLATE (RFC 1951), gzip (RFC 1952), and zlib (RFC 1950) encoder and decoder.

- `no_std` (requires only `alloc`)
- No `unsafe` code (`#![forbid(unsafe_code)]`)
- Sans-io: the library performs no I/O itself — callers feed bytes in and consume bytes out, making it usable with any I/O strategy
- WebSocket permessage-deflate ([RFC 7692](https://www.rfc-editor.org/rfc/rfc7692)) support

Examples
--------

### One-shot DEFLATE

```rust
let input = b"Hello, DEFLATE!";
let compressed = noflate::deflate::compress(input)?;
let decompressed = noflate::deflate::decompress(&compressed)?;
assert_eq!(decompressed, input);
```

### Streaming encoder

```rust
let mut encoder = noflate::deflate::Encoder::new();
encoder.feed(b"Hello, ")?;
encoder.feed(b"world!")?;
encoder.finish()?;
let compressed = encoder.output().to_vec();
encoder.advance(compressed.len());
assert_eq!(noflate::deflate::decompress(&compressed)?, b"Hello, world!");
```

### Streaming decoder

```rust
let compressed = noflate::deflate::compress(b"hello")?;

let mut decoder = noflate::deflate::Decoder::new();
decoder.feed(&compressed)?;
let out = decoder.output().to_vec();
decoder.advance(out.len());
assert!(decoder.is_finished());
assert_eq!(out, b"hello");
```

### gzip / zlib

```rust
let gz = noflate::gzip::compress(b"hello world")?;
assert_eq!(noflate::gzip::decompress(&gz)?, b"hello world");

let zl = noflate::zlib::compress(b"hello world")?;
assert_eq!(noflate::zlib::decompress(&zl)?, b"hello world");
```

### `std::io::{Read, Write}` interop

noflate performs no I/O itself, but the sans-io API plugs into
`std::io::Write` and `std::io::Read` with a small adapter — `Write::write`
forwards to `feed` and drains `output` into the inner sink, and
`Read::read` pulls from `output` and tops up the decoder via `feed` from
the inner source. See [`examples/io_bridge.rs`](examples/io_bridge.rs)
for a runnable `DeflateWriter` / `DeflateReader` pair; the same pattern
works verbatim for `gzip` and `zlib`.

### WebSocket permessage-deflate (RFC 7692)

Supported via [`Encoder::sync_flush`](https://docs.rs/noflate/latest/noflate/deflate/struct.Encoder.html#method.sync_flush)
and [`Encoder::reset_history`](https://docs.rs/noflate/latest/noflate/deflate/struct.Encoder.html#method.reset_history).
See their docs for the sender/receiver pattern.

Benchmarks
----------

The repository ships three benchmark binaries:

```sh
BENCH_REPEATS=30 cargo run --release --example bench
BENCH_REPEATS=30 cargo run --release --example bench_checksum
BENCH_REPEATS=30 cargo run --release --example bench_wrappers
```

The numbers below are **rough indicators only** — throughput fluctuates substantially with hardware, runner load, workload size, and specific input. Depending on the environment, noflate can be faster or slower than `flate2` on the same operation. Re-run the [`Benchmark`](.github/workflows/benchmark.yml) workflow (Actions → Benchmark → Run workflow) or run the examples locally before making performance-sensitive decisions.

- Commit: [`7ef5eec`](https://github.com/sile/noflate/commit/7ef5eec)
- Source: GitHub Actions, standard runners
  - `ubuntu-latest`: AMD EPYC 7763 (Milan, Zen 3) or 9V74 (Genoa, Zen 4); rarely Intel Xeon Platinum 8370C (Ice Lake) — 4 vCPU x86_64 Azure VMs; the runner pool mixes SKUs
  - `macos-latest`: Apple M1 Virtual (3 vCPU, arm64)
- Toolchain: `rustc 1.95.0`, `--release`
- Methodology: `BENCH_REPEATS=30` per run (best-of reported); aggregated across 40 workflow runs by median within each CPU SKU (23× EPYC 7763, 15× EPYC 9V74, 40× M1; 2× Intel Xeon runs omitted — too few samples). Median rather than best-of across runs because runner load varies and best-of would cherry-pick lucky-fast instances. Encode throughput is of the raw input; decode throughput is of the decompressed output.

**DEFLATE, 1 MiB English text** (MB/s):

| platform                    | noflate enc | flate2 enc | noflate dec | flate2 dec |
|-----------------------------|------------:|-----------:|------------:|-----------:|
| ubuntu — EPYC 7763 (Zen 3)  |         427 |        363 |        6652 |       3557 |
| ubuntu — EPYC 9V74 (Zen 4)  |         390 |        498 |        7080 |       3485 |
| macos — M1 (Virtual)        |         600 |        994 |        7395 |       2977 |

**Encode compression ratio** (`compressed / original` — deterministic, identical across runners):

| input          | noflate  | flate2   |
|----------------|---------:|---------:|
| english 1 KiB  |   0.1494 |   0.1504 |
| english 64 KiB |   0.0064 |   0.0065 |
| english 1 MiB  |   0.0040 |   0.0040 |
| zeros 64 KiB   |   0.0012 |   0.0012 |
| random 64 KiB  |   1.0011 |   1.0002 |

Noflate's ratio is within ~0.1 % of `flate2` across the board — slightly better on short text (more thorough length-limited Huffman), slightly worse on ultra-short stored payloads (e.g. 64 KiB of zeros: 79 bytes vs 78 bytes) and on incompressible input.

**Checksums of 1 MiB** (MB/s):

| platform                    | noflate CRC-32 | crc32fast | noflate Adler-32 | adler32 crate |
|-----------------------------|---------------:|----------:|-----------------:|--------------:|
| ubuntu — EPYC 7763 (Zen 3)  |           2256 |     12172 |             3037 |          3007 |
| ubuntu — EPYC 9V74 (Zen 4)  |           2006 |     10800 |             2804 |          2705 |
| macos — M1 (Virtual)        |           3045 |      7968 |             2763 |          2661 |

Notes on these numbers:

- The DEFLATE **decoder** is consistently faster than `flate2` on text — about 1.9× on EPYC 7763, 2.0× on EPYC 9V74, and 2.5× on macOS for the 1 MiB case. Random data also favours noflate. See the raw workflow logs for the full matrix.
- The DEFLATE **encoder** picture is CPU-dependent: on EPYC 7763 noflate is ~1.2× faster than `flate2` on 1 MiB English text, but on the newer EPYC 9V74 (~0.8×) and on macOS M1 (~0.6×) it's slower — `flate2`'s 1 MiB encode benefits more from newer ISAs than noflate does. Per-call setup cost dominates near 1 KiB inputs (3–5× slower than `flate2` across all SKUs).
- **Adler-32** matches the `adler32` crate on every runner (within ~5 %).
- **CRC-32** is ~5× slower than `crc32fast`'s PCLMULQDQ path on x86_64 and ~2.6× slower on macOS M1 — the price of staying portable, safe (`#![forbid(unsafe_code)]`), and free of CPU-specific intrinsics.

References
----------

- DEFLATE: [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951)
- ZLIB: [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
- GZIP: [RFC 1952](https://www.rfc-editor.org/rfc/rfc1952)
- WebSocket per-message compression: [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692)
