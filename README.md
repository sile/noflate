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

- Commit: [`8da4e92`](https://github.com/sile/noflate/commit/8da4e92ee99b2a37596b2877ad98e58dcc486593) (median of 5 workflow runs)
- Source: GitHub Actions, standard runners
  - `ubuntu-latest`: AMD EPYC 7763 (2 vCPU, x86_64, Azure)
  - `macos-latest`: Apple M1 Virtual (3 vCPU, arm64)
- Toolchain: `rustc 1.95.0`, `--release`
- Methodology: `BENCH_REPEATS=30`, best-of reported; encode throughput is of the raw input stream, decode throughput is of the decompressed output stream

**DEFLATE, 1 MiB English text** (MB/s):

|        | noflate (ubuntu) | flate2 (ubuntu) | noflate (macos) | flate2 (macos) |
|--------|-----------------:|----------------:|----------------:|---------------:|
| encode |              434 |             363 |             625 |           1034 |
| decode |             6727 |            3498 |            5866 |           3172 |

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

|          | noflate (ubuntu) | reference (ubuntu)     | noflate (macos) | reference (macos)    |
|----------|-----------------:|-----------------------:|----------------:|---------------------:|
| CRC-32   |             2257 |   11501 (`crc32fast`)  |            3045 |   7969 (`crc32fast`) |
| Adler-32 |             3038 |      3013 (`adler32`)  |            2765 |     2665 (`adler32`) |

Notes on these numbers:

- The DEFLATE **decoder** is consistently faster than `flate2` on text — roughly 2× on both runners for the 1 MiB case. Random data also favours noflate. See the raw workflow logs for the full matrix.
- The DEFLATE **encoder** is competitive with `flate2` on long text — faster on Ubuntu (~1.2×) but slower on macOS (~0.6×); per-call setup cost dominates near 1 KiB inputs (4–5× slower there).
- **Adler-32** matches the `adler32` crate on both runners.
- **CRC-32** is 2.5×–5× slower than `crc32fast`'s PCLMULQDQ path — the price of staying portable, safe (`#![forbid(unsafe_code)]`), and free of CPU-specific intrinsics.

References
----------

- DEFLATE: [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951)
- ZLIB: [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
- GZIP: [RFC 1952](https://www.rfc-editor.org/rfc/rfc1952)
- WebSocket per-message compression: [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692)
