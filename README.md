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
- Sans-I/O streaming API — the library owns its buffers; the caller feeds bytes in and pulls bytes out. No `std::io::Read` / `std::io::Write` coupling, no implicit I/O.
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

The repository ships two benchmark binaries:

```sh
BENCH_REPEATS=30 cargo run --release --example bench
BENCH_REPEATS=30 cargo run --release --example bench_checksum
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
| encode |              402 |             363 |             603 |            994 |
| decode |             1839 |            3230 |            4743 |           3019 |

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
| CRC-32   |             2258 |    11510 (`crc32fast`) |            3053 |   7959 (`crc32fast`) |
| Adler-32 |             3038 |      3004 (`adler32`)  |            2849 |     2662 (`adler32`) |

Notes on these numbers:

- The DEFLATE **decoder** is usually faster than `flate2` on text — but not always. The 1 MiB case on the Ubuntu runner is the exception (1.8× slower); smaller sizes and random data favour noflate. See the raw workflow logs for the full matrix.
- The DEFLATE **encoder** is within ~2× of `flate2` on long text; per-call setup cost dominates near 1 KiB inputs (4–5× slower there).
- **Adler-32** matches the `adler32` crate on both runners.
- **CRC-32** is 2.5×–5× slower than `crc32fast`'s PCLMULQDQ path — the price of staying portable, safe (`#![forbid(unsafe_code)]`), and free of CPU-specific intrinsics.

References
----------

- DEFLATE: [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951)
- ZLIB: [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
- GZIP: [RFC 1952](https://www.rfc-editor.org/rfc/rfc1952)
