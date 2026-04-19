---
name: noflate
description: >
  Work with the noflate Rust crate: use DEFLATE, gzip, and zlib
  encoding/decoding APIs; wire sans-io streaming encoders and decoders; use
  EncodeOptions; handle WebSocket permessage-deflate (RFC 7692); and debug
  no_std or checksum-related behavior. Use when the task mentions noflate,
  DEFLATE/gzip/zlib internals, sync_flush, reset_history, Format::detect, or
  streaming compression with this crate.
license: MIT
compatibility: Requires Rust 1.88+ and cargo.
metadata:
  author: sile
  version: "0.0.7"
---

# noflate

Use this skill when integrating `noflate` into Rust code. Focus on the crate's
actual API shape and usage patterns, not generic compression background.

## What this crate exposes

- `noflate::deflate` for raw DEFLATE (RFC 1951)
- `noflate::gzip` for GZIP (RFC 1952), plus `Crc32` / `crc32`
- `noflate::zlib` for ZLIB (RFC 1950), plus `Adler32` / `adler32`
- Shared root types: `Error`, `Result`, `Format`

Each format module exposes the same main shape:

- One-shot helpers: `compress(data) -> Result<Vec<u8>>`, `decompress(data) -> Result<Vec<u8>>`
- Streaming types: `Encoder`, `Decoder`

## Choosing the right API

1. Pick `deflate`, `gzip`, or `zlib` based on the wire format you need.
2. Use `compress` / `decompress` for one-shot data.
3. Use `Encoder` / `Decoder` when data arrives incrementally or output must be
   consumed in chunks.
4. Use `EncodeOptions` when you need fixed Huffman blocks, stored blocks, or
   buffering control.

## Usage gotchas

- This crate is `#![no_std]`; it requires `alloc`, not `std`.
- The streaming API is sans-io:
  `feed(data)` pushes input, `output()` borrows produced bytes, `advance(n)`
  marks bytes as consumed.
- One-shot compression uses buffered input internally; for similar behavior in
  streaming code, use `EncodeOptions::new().buffer_all_input()`.
- `sync_flush()` is for continuing a DEFLATE stream across message boundaries.
  It is useful for `permessage-deflate`; do not treat it like `finish()`.
- `reset_history()` is only for `no_context_takeover` style flows. Do not reset
  state between messages unless the protocol requires it.
- `Format::detect(&data)` needs enough prefix bytes to distinguish framing; it
  returns `None` for too-short input.
- Checksum helpers are format-specific: gzip uses CRC-32, zlib uses Adler-32.

## API patterns to preserve

One-shot:

```rust
let compressed = noflate::deflate::compress(b"hello")?;
let decompressed = noflate::deflate::decompress(&compressed)?;
```

Streaming encoder:

```rust
let mut enc = noflate::deflate::Encoder::new();
enc.feed(b"chunk1")?;
enc.feed(b"chunk2")?;
enc.finish()?;
let out = enc.output().to_vec();
enc.advance(out.len());
```

Streaming decoder:

```rust
let mut dec = noflate::deflate::Decoder::new();
dec.feed(&compressed)?;
let out = dec.output().to_vec();
dec.advance(out.len());
assert!(dec.is_finished());
```

Options:

```rust
use noflate::deflate::EncodeOptions;

let dynamic = EncodeOptions::new();
let fixed = EncodeOptions::new().fixed_huffman();
let stored = EncodeOptions::new().stored();
let buffered = EncodeOptions::new().buffer_all_input();
let limited = EncodeOptions::new().max_block_input_bytes(32 * 1024);
```

WebSocket permessage-deflate:

```rust
let mut enc = noflate::deflate::Encoder::new();
enc.feed(message)?;
enc.sync_flush()?;
let frame = enc.output().to_vec();
enc.advance(frame.len());
// Per RFC 7692, strip the trailing 0x00 0x00 0xFF 0xFF before sending.

enc.reset_history(); // only when no_context_takeover is negotiated
```

Format detection:

```rust
match noflate::Format::detect(&data) {
    Some(noflate::Format::Gzip) => {}
    Some(noflate::Format::Zlib) => {}
    Some(noflate::Format::Deflate) => {}
    None => {}
}
```

## Practical hints

- If you already have all input bytes, start with `compress` / `decompress`
  before reaching for the streaming API.
- If you use the streaming API, always drain `output()` and call `advance()`
  after consuming bytes.
- For WebSocket `permessage-deflate`, strip the final `0x00 0x00 0xFF 0xFF`
  after `sync_flush()`.
- `EncodeOptions` belongs to `noflate::deflate`, but the same options are used
  by `gzip::Encoder` and `zlib::Encoder`.
