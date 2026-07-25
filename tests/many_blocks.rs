//! Regression test for the same problem class as libflate #88
//! (https://github.com/sile/libflate/pull/89): a DEFLATE stream carrying
//! many blocks — in particular many empty non-final stored blocks — must
//! decode without exhausting the thread stack. In libflate the failure
//! mode was self-recursive tail calls in `Read for Decoder` that added
//! one stack frame per block. noflate's decoders drive a state machine
//! with an internal `loop`, so the same payload should decode uneventfully;
//! this test locks that in.
//!
//! The payload is a `blocks - 1` chain of empty non-final stored blocks
//! followed by one final stored block carrying the minimal valid WASM
//! module, so the decoded bytes are both small and recognizable.

use noflate::gzip::Crc32;

/// Minimal valid WebAssembly module — chosen only so the decoded bytes
/// look like something.
const WASM: [u8; 8] = [0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00];

/// 250_000 blocks matches libflate's `test_issue_88`. On master libflate
/// (before PR #89) this count reliably blew a default 8 MiB thread stack.
const BLOCKS: usize = 250_000;

/// Build a raw DEFLATE stream: `blocks - 1` empty non-final stored blocks
/// followed by one final stored block carrying [`WASM`].
fn make_deflate_stream(blocks: usize) -> Vec<u8> {
    assert!(blocks >= 1, "need at least the final payload block");
    /// BFINAL=0, BTYPE=00, then byte-aligned LEN=0, NLEN=0xFFFF.
    const EMPTY_NONFINAL_STORED_BLOCK: [u8; 5] = [0x00, 0x00, 0x00, 0xFF, 0xFF];

    let len = WASM.len() as u16;
    let mut out = Vec::with_capacity((blocks - 1) * 5 + 5 + WASM.len());
    for _ in 0..(blocks - 1) {
        out.extend_from_slice(&EMPTY_NONFINAL_STORED_BLOCK);
    }
    // Final stored block: BFINAL=1, BTYPE=00 → 0x01, then LEN, NLEN, bytes.
    out.push(0x01);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(&WASM);
    out
}

/// Wrap [`make_deflate_stream`] in a minimal gzip envelope.
fn make_gzip_stream(blocks: usize) -> Vec<u8> {
    const HEADER: [u8; 10] = [0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF];
    let body = make_deflate_stream(blocks);
    let mut crc = Crc32::new();
    crc.update(&WASM);

    let mut out = Vec::with_capacity(HEADER.len() + body.len() + 8);
    out.extend_from_slice(&HEADER);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc.value().to_le_bytes());
    out.extend_from_slice(&(WASM.len() as u32).to_le_bytes());
    out
}

#[test]
fn deflate_decodes_many_empty_blocks() {
    let stream = make_deflate_stream(BLOCKS);
    let decoded = noflate::deflate::decompress(&stream).unwrap();
    assert_eq!(decoded, WASM);
}

#[test]
fn gzip_decodes_many_empty_blocks() {
    let stream = make_gzip_stream(BLOCKS);
    let decoded = noflate::gzip::decompress(&stream).unwrap();
    assert_eq!(decoded, WASM);
}

#[test]
fn deflate_streaming_decodes_many_empty_blocks() {
    // Same payload, but fed to the streaming decoder in small chunks so
    // the drive loop is entered many times across block boundaries.
    let stream = make_deflate_stream(BLOCKS);
    let mut decoder = noflate::deflate::Decoder::new();
    for chunk in stream.chunks(4096) {
        decoder.feed(chunk).unwrap();
    }
    assert!(decoder.is_finished());
    let out = decoder.output().to_vec();
    decoder.advance(out.len());
    assert_eq!(out, WASM);
}
