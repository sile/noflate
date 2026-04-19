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

### WebSocket permessage-deflate (RFC 7692)

`Encoder::sync_flush` frames messages over a single DEFLATE stream by
appending an empty `BFINAL=0` stored block that byte-aligns the output
with the 4-byte trailer `0x00 0x00 0xFF 0xFF`. `Encoder::reset_history`
drops the LZ77 sliding window so the next message emits no
back-references into the previous one — the requirement behind
`*_no_context_takeover` in [RFC 7692 §7.1.1](https://www.rfc-editor.org/rfc/rfc7692#section-7.1.1).

```rust
// Sender (per RFC 7692 §7.2.1).
let mut encoder = noflate::deflate::Encoder::new();
encoder.feed(b"hello")?;
encoder.sync_flush()?;
let mut frame = encoder.output().to_vec();
encoder.advance(frame.len());
assert!(frame.ends_with(&[0x00, 0x00, 0xFF, 0xFF]));
frame.truncate(frame.len() - 4);       // strip the trailer per RFC
// ... send `frame` as the WebSocket payload ...
encoder.reset_history();               // only if no_context_takeover

// Receiver (per RFC 7692 §7.2.2): append the stripped trailer back.
let mut decoder = noflate::deflate::Decoder::new();
decoder.feed(&frame)?;
decoder.feed(&[0x00, 0x00, 0xFF, 0xFF])?;
let message = decoder.output().to_vec();
decoder.advance(message.len());
assert_eq!(message, b"hello");
```

The decoder needs no explicit reset: the sender guarantees no
back-reference crosses a message boundary under `*_no_context_takeover`,
so subsequent frames can be fed into the same `Decoder` instance.

Benchmarks
----------

The repository ships three benchmark binaries:

```sh
BENCH_REPEATS=30 cargo run --release --example bench
BENCH_REPEATS=30 cargo run --release --example bench_checksum
BENCH_REPEATS=30 cargo run --release --example bench_wrappers
```

The numbers below are **rough indicators only** — throughput fluctuates substantially with hardware, runner load, workload size, and specific input. Depending on the environment, noflate can be faster or slower than `flate2` on the same operation. Re-run the [`Benchmark`](.github/workflows/benchmark.yml) workflow (Actions → Benchmark → Run workflow) or run the examples locally before making performance-sensitive decisions.

- Source: GitHub Actions, standard runners
  - `ubuntu-latest`: AMD EPYC 7763 (2 vCPU, x86_64, Azure)
  - `macos-latest`: Apple M1 Virtual (3 vCPU, arm64)
- Toolchain: `rustc 1.94.1`, `--release`
- Methodology: `BENCH_REPEATS=30`, best-of reported; encode throughput is of the raw input stream, decode throughput is of the decompressed output stream

**DEFLATE, 1 MiB English text** (MB/s):

|        | noflate (ubuntu) | flate2 (ubuntu) | noflate (macos) | flate2 (macos) |
|--------|-----------------:|----------------:|----------------:|---------------:|
| encode |              427 |             363 |             645 |           1067 |
| decode |             6564 |            3166 |            7111 |           3617 |

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
| CRC-32   |             2259 |   12178 (`crc32fast`)  |            3275 |   8545 (`crc32fast`) |
| Adler-32 |             3037 |      3020 (`adler32`)  |            2965 |     2858 (`adler32`) |

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
