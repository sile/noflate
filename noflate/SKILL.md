---
name: noflate
description: >
  Work with the noflate crate — a zero-dependency, no_std, sans-io DEFLATE/gzip/zlib
  encoder and decoder in Rust. Use when writing code that compresses or decompresses
  data with noflate, implementing streaming compression, using WebSocket
  permessage-deflate (RFC 7692), or troubleshooting noflate API usage.
license: MIT
compatibility: Requires Rust 1.88+ and cargo
metadata:
  author: sile
  version: "0.0.3"
---

# noflate

A zero-dependency DEFLATE (RFC 1951), gzip (RFC 1952), and zlib (RFC 1950)
encoder and decoder. `no_std` compatible (requires only `alloc`), no `unsafe`
code.

## Module structure

All three formats expose the same API shape:

- `noflate::deflate` — raw DEFLATE (RFC 1951)
- `noflate::gzip` — GZIP container (RFC 1952); also provides `Crc32` / `crc32`
- `noflate::zlib` — ZLIB container (RFC 1950); also provides `Adler32` / `adler32`

Each module contains:

| Item | Description |
|------|-------------|
| `compress(data) -> Result<Vec<u8>>` | One-shot compression |
| `decompress(data) -> Result<Vec<u8>>` | One-shot decompression |
| `Encoder` | Streaming encoder (`new`, `with_options`, `feed`, `finish`, `output`, `advance`, `is_finished`) |
| `Decoder` | Streaming decoder (`new`, `feed`, `output`, `advance`, `is_finished`) |

Root-level shared types: `Error`, `Result`, `Format`.

## Sans-io pattern

The library performs no I/O. Callers drive encoding/decoding with three
operations:

1. **`feed(data)`** — push bytes into the encoder or decoder
2. **`output()`** — borrow produced bytes
3. **`advance(n)`** — mark `n` bytes as consumed

## One-shot usage

```rust
let compressed = noflate::deflate::compress(b"hello")?;
let decompressed = noflate::deflate::decompress(&compressed)?;
// Same pattern for noflate::gzip and noflate::zlib
```

## Streaming usage

Encoder:

```rust
let mut enc = noflate::deflate::Encoder::new();
enc.feed(b"chunk1")?;
enc.feed(b"chunk2")?;
enc.finish()?;
let compressed = enc.output().to_vec();
enc.advance(compressed.len());
```

Decoder:

```rust
let mut dec = noflate::deflate::Decoder::new();
dec.feed(&compressed)?;
let out = dec.output().to_vec();
dec.advance(out.len());
assert!(dec.is_finished());
```

## Encoder options

`EncodeOptions` (in `noflate::deflate`) configures the DEFLATE layer for all
three formats:

```rust
// Dynamic Huffman (default)
noflate::deflate::Encoder::new();

// Fixed Huffman
noflate::deflate::Encoder::with_options(
    noflate::deflate::EncodeOptions::new().fixed_huffman()
);

// Stored (uncompressed)
noflate::deflate::Encoder::with_options(
    noflate::deflate::EncodeOptions::new().stored()
);

// Buffer all input (one block, best for one-shot)
noflate::deflate::EncodeOptions::new().buffer_all_input();

// Custom block size
noflate::deflate::EncodeOptions::new().max_block_input_bytes(32 * 1024);

// Options work with gzip/zlib too
noflate::gzip::Encoder::with_options(
    noflate::deflate::EncodeOptions::new().fixed_huffman()
);
```

## WebSocket permessage-deflate (RFC 7692)

`deflate::Encoder` supports `sync_flush` and `reset_history` for framing
messages over a single DEFLATE stream:

```rust
let mut enc = noflate::deflate::Encoder::new();

// Per message:
enc.feed(message)?;
enc.sync_flush()?;
let frame = enc.output().to_vec();
enc.advance(frame.len());
// frame ends with 0x00 0x00 0xFF 0xFF — strip before sending per RFC

// If no_context_takeover is negotiated:
enc.reset_history();
```

## Format detection

```rust
let format = noflate::Format::detect(&data);
// Returns Some(Format::Gzip), Some(Format::Zlib), or Some(Format::Deflate)
// Returns None if data is shorter than 2 bytes
```

## Conventions when modifying this crate

- `#![no_std]` — use `alloc` types only; `std` is only available in `#[cfg(test)]`
- `#![forbid(unsafe_code)]` — no `unsafe` anywhere
- Parameter naming: use `data` for all `feed`/`compress`/`decompress` parameters
- Doc style: "One-shot: ..." for convenience functions; short one-liners for methods
- Error handling: `noflate::Result<T>` alias; `Error::InvalidData` and `Error::Unsupported`
- Testing: `cargo test`, `cargo doc`, `cargo clippy`; fuzz targets in `fuzz/`, property tests in `pbt/`
