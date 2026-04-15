//! Tests that streaming `feed`-in-chunks yields identical results to
//! feeding the whole input at once.

use noflate::{Decoder, Encoder, compress, decompress};

fn decode_byte_by_byte(compressed: &[u8]) -> Vec<u8> {
    let mut d = Decoder::new();
    let mut collected = Vec::new();
    for &byte in compressed {
        d.feed(&[byte]).unwrap();
        let out = d.output().to_vec();
        collected.extend_from_slice(&out);
        d.advance(out.len());
    }
    assert!(d.is_finished(), "stream did not finish");
    collected
}

fn decode_in_chunks(compressed: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut d = Decoder::new();
    let mut collected = Vec::new();
    let mut offset = 0;
    while offset < compressed.len() {
        let end = (offset + chunk_size).min(compressed.len());
        d.feed(&compressed[offset..end]).unwrap();
        let out = d.output().to_vec();
        collected.extend_from_slice(&out);
        d.advance(out.len());
        offset = end;
    }
    assert!(d.is_finished());
    collected
}

fn encode_byte_by_byte(input: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    for &byte in input {
        e.feed(&[byte]).unwrap();
    }
    e.finish().unwrap();
    let out = e.output().to_vec();
    e.advance(out.len());
    out
}

#[test]
fn decoder_byte_by_byte_matches_whole() {
    let inputs: &[&[u8]] = &[
        b"",
        b"a",
        b"Hello World!",
        b"banana banana banana banana",
        &[0u8; 512],
    ];
    for input in inputs {
        let compressed = compress(input).unwrap();
        let out = decode_byte_by_byte(&compressed);
        assert_eq!(out, *input, "incremental decode mismatch");
    }
}

#[test]
fn decoder_random_chunk_sizes() {
    let input = b"The quick brown fox jumps over the lazy dog.";
    let compressed = compress(input).unwrap();
    for chunk_size in [1, 2, 3, 5, 7, 11] {
        let out = decode_in_chunks(&compressed, chunk_size);
        assert_eq!(out, input, "chunk_size={chunk_size}");
    }
}

#[test]
fn encoder_byte_by_byte_matches_whole() {
    let inputs: &[&[u8]] = &[b"", b"X", b"Hello World!", b"banana banana"];
    for input in inputs {
        let compressed = encode_byte_by_byte(input);
        let decoded = decompress(&compressed).unwrap();
        assert_eq!(decoded, *input);
    }
}

#[test]
fn decoder_advance_mid_stream() {
    // The caller can drain output incrementally rather than all at once.
    let input = b"The quick brown fox jumps over the lazy dog.";
    let compressed = compress(input).unwrap();
    let mut d = Decoder::new();
    let mut collected = Vec::new();
    for chunk in compressed.chunks(3) {
        d.feed(chunk).unwrap();
        // Drain only half of what's available each time.
        let half = d.output().len() / 2;
        collected.extend_from_slice(&d.output()[..half]);
        d.advance(half);
    }
    collected.extend_from_slice(d.output());
    d.advance(d.output().len());
    assert_eq!(collected, input);
    assert!(d.is_finished());
}
