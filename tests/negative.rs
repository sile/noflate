//! Negative tests for the decoder.

use noflate::deflate::{Decoder, compress, decompress};

#[test]
fn reserved_block_type_errors() {
    // BFINAL=1, BTYPE=0b11 (reserved).
    let input = [0x07];
    assert!(decompress(&input).is_err());
}

#[test]
fn truncated_stream_is_not_an_error() {
    let compressed = compress(b"hello world").unwrap();
    let truncated = &compressed[..compressed.len() / 2];
    let mut d = Decoder::new();
    d.feed(truncated).expect("truncated feed is not an error");
    assert!(!d.is_finished());
}

#[test]
fn feeding_after_finish_errors() {
    let compressed = compress(b"abc").unwrap();
    let mut d = Decoder::new();
    d.feed(&compressed).unwrap();
    assert!(d.is_finished());
    assert!(d.feed(b"extra").is_err());
}

#[test]
fn decompress_missing_final_block_marker() {
    // Partial compressed stream — decompress should say the stream ended
    // before the final block.
    let compressed = compress(b"abc").unwrap();
    let truncated = &compressed[..compressed.len() - 1];
    assert!(decompress(truncated).is_err());
}

#[test]
#[should_panic]
fn advance_past_end_panics() {
    let compressed = compress(b"hi").unwrap();
    let mut d = Decoder::new();
    d.feed(&compressed).unwrap();
    let len = d.output().len();
    d.advance(len + 1);
}
