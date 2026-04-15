noflate
=======

[![noflate](https://img.shields.io/crates/v/noflate.svg)](https://crates.io/crates/noflate)
[![Documentation](https://docs.rs/noflate/badge.svg)](https://docs.rs/noflate)
[![Actions Status](https://github.com/sile/noflate/workflows/CI/badge.svg)](https://github.com/sile/noflate/actions)
![License](https://img.shields.io/crates/l/noflate)

A sans-io DEFLATE / ZLIB / GZIP encoder and decoder with no dependencies.

Features
--------

- No dependencies; pure, portable, safe Rust
- Sans-io streaming API: the library owns its buffers; the caller feeds bytes in and pulls bytes out without any `std::io::Read` / `std::io::Write` coupling
- DEFLATE (RFC 1951): encoder and decoder, all three block kinds (stored / fixed Huffman / dynamic Huffman)
- ZLIB (RFC 1950) wrapper with Adler-32 verification
- GZIP (RFC 1952) wrapper with CRC-32 + ISIZE verification
- Streaming [`Adler32`] and [`Crc32`] primitives (slice-by-16 CRC-32)
- Binary-compatible with `flate2` in both directions

[`Adler32`]: https://docs.rs/noflate/latest/noflate/struct.Adler32.html
[`Crc32`]: https://docs.rs/noflate/latest/noflate/struct.Crc32.html

Examples
--------

### One-shot DEFLATE

```rust
let input = b"Hello, DEFLATE!";
let compressed = noflate::compress(input).unwrap();
let decompressed = noflate::decompress(&compressed).unwrap();
assert_eq!(decompressed, input);
```

### Streaming decoder

```rust
let compressed = noflate::compress(b"hello").unwrap();

let mut decoder = noflate::Decoder::new();
decoder.feed(&compressed).unwrap();
assert!(decoder.is_finished());

let out = decoder.output().to_vec();
decoder.advance(out.len());
assert_eq!(out, b"hello");
```

### zlib / gzip

```rust
let gz = noflate::gzip::compress(b"hello world").unwrap();
assert_eq!(noflate::gzip::decompress(&gz).unwrap(), b"hello world");

let zl = noflate::zlib::compress(b"hello world").unwrap();
assert_eq!(noflate::zlib::decompress(&zl).unwrap(), b"hello world");
```

### Checksums

```rust
assert_eq!(noflate::crc32(b"a"), 0xE8B7_BE43);
assert_eq!(noflate::adler32(b"a"), 0x0062_0062);
```

Benchmarks
----------

Two benchmark binaries compare noflate against `flate2`, `crc32fast`, and the `adler32` crate:

```sh
BENCH_REPEATS=20 cargo run --release --example bench
BENCH_REPEATS=20 cargo run --release --example bench_checksum
```

The numbers below are from one specific run on one machine and are included only to give a rough sense of where noflate sits relative to the reference crates. Re-run the benchmarks on your own hardware for decisions that matter.

- Hardware: Apple M1 Max (10 cores, arm64), macOS 15 (Darwin 24.6.0)
- Toolchain: `rustc 1.94.1 (e408947bf 2026-03-25)`, release build, LTO off
- Methodology: `BENCH_REPEATS=20`, best-of reported; throughput is of the decompressed stream for decode and of the raw input stream for encode / checksums

| workload                   | noflate    | reference                      |
|----------------------------|------------|--------------------------------|
| DEFLATE decode, 1 MiB text | 8551 MB/s  | 3477 MB/s (`flate2`)           |
| DEFLATE encode, 1 MiB text |  649 MB/s  | 1076 MB/s (`flate2`)           |
| CRC-32 of 1 MiB            | 3100 MB/s  | 8100 MB/s (`crc32fast`, CLMUL) |
| Adler-32 of 1 MiB          | 2437 MB/s  | 2588 MB/s (`adler32`)          |

The DEFLATE decoder beats `flate2` on most text workloads. The encoder is within about 2× on long text inputs. CRC-32 is roughly 40 % of `crc32fast`'s hardware-accelerated PCLMULQDQ path — that is the price of staying portable, safe (`#![forbid(unsafe_code)]`-compatible), and free of CPU-specific intrinsics.

References
----------

- DEFLATE: [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951)
- ZLIB: [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
- GZIP: [RFC 1952](https://www.rfc-editor.org/rfc/rfc1952)
