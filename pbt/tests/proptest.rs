//! Property-based tests for noflate.
//!
//! Properties covered:
//!
//! - Self-roundtrip for DEFLATE / ZLIB / GZIP across all three encoder
//!   block kinds (stored / fixed Huffman / dynamic Huffman).
//! - Chunked feed equivalence for both decoder and encoder (feeding the
//!   input in arbitrary-sized slices must produce identical output).
//! - Binary interoperability with `flate2` in both directions.
//! - Checksum agreement with the `adler32` and `crc32fast` crates.
//!
//! Tests cap input length at 64 KiB to keep proptest runs fast; the
//! properties don't depend on very large inputs.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
use noflate::deflate::{EncodeOptions, Encoder};
use proptest::prelude::*;

fn compress_with(opts: EncodeOptions, input: &[u8]) -> Vec<u8> {
    let mut e = Encoder::with_options(opts);
    e.feed(input).expect("encoder feed");
    e.finish().expect("encoder finish");
    let out = e.output().to_vec();
    e.advance(out.len());
    out
}

fn flate2_deflate(data: &[u8]) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_inflate(data: &[u8]) -> Vec<u8> {
    let mut d = DeflateDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn flate2_zlib_encode(data: &[u8]) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_zlib_decode(data: &[u8]) -> Vec<u8> {
    let mut d = ZlibDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn flate2_gzip_encode(data: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_gzip_decode(data: &[u8]) -> Vec<u8> {
    let mut d = GzDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn chunked_decoder_output(compressed: &[u8], chunks: &[usize]) -> Vec<u8> {
    let mut d = noflate::deflate::Decoder::new();
    let mut collected = Vec::new();
    let mut offset = 0;
    for &chunk in chunks {
        if offset >= compressed.len() {
            break;
        }
        let end = (offset + chunk).min(compressed.len());
        d.feed(&compressed[offset..end]).expect("feed");
        let out = d.output().to_vec();
        collected.extend_from_slice(&out);
        d.advance(out.len());
        offset = end;
    }
    if offset < compressed.len() {
        d.feed(&compressed[offset..]).expect("tail feed");
        let out = d.output().to_vec();
        collected.extend_from_slice(&out);
        d.advance(out.len());
    }
    collected
}

fn chunked_encoder_output(input: &[u8], chunks: &[usize]) -> Vec<u8> {
    let mut e = Encoder::new();
    let mut offset = 0;
    for &chunk in chunks {
        if offset >= input.len() {
            break;
        }
        let end = (offset + chunk).min(input.len());
        e.feed(&input[offset..end]).expect("feed");
        offset = end;
    }
    if offset < input.len() {
        e.feed(&input[offset..]).expect("tail feed");
    }
    e.finish().expect("finish");
    let out = e.output().to_vec();
    e.advance(out.len());
    out
}

fn bounded_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..=64 * 1024)
}

fn chunk_sizes() -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(1usize..=128, 1..=64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn deflate_dynamic_roundtrip(input in bounded_bytes()) {
        let compressed = compress_with(EncodeOptions::new(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn deflate_fixed_roundtrip(input in bounded_bytes()) {
        let compressed = compress_with(EncodeOptions::new().fixed_huffman(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn deflate_stored_roundtrip(input in bounded_bytes()) {
        let compressed = compress_with(EncodeOptions::new().stored(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn zlib_roundtrip(input in bounded_bytes()) {
        let compressed = noflate::zlib::compress(&input).expect("compress");
        let decompressed = noflate::zlib::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn gzip_roundtrip(input in bounded_bytes()) {
        let compressed = noflate::gzip::compress(&input).expect("compress");
        let decompressed = noflate::gzip::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn decoder_chunked_feed_matches_whole(
        input in bounded_bytes(),
        chunks in chunk_sizes(),
    ) {
        let compressed = noflate::deflate::compress(&input).expect("compress");
        let out = chunked_decoder_output(&compressed, &chunks);
        prop_assert_eq!(out, input);
    }

    #[test]
    fn encoder_chunked_feed_roundtrips(
        input in bounded_bytes(),
        chunks in chunk_sizes(),
    ) {
        let compressed = chunked_encoder_output(&input, &chunks);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn noflate_decompresses_flate2_deflate(input in bounded_bytes()) {
        let compressed = flate2_deflate(&input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn flate2_decompresses_noflate_deflate(input in bounded_bytes()) {
        let compressed = noflate::deflate::compress(&input).expect("compress");
        let decompressed = flate2_inflate(&compressed);
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn noflate_decompresses_flate2_zlib(input in bounded_bytes()) {
        let compressed = flate2_zlib_encode(&input);
        let decompressed = noflate::zlib::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn flate2_decompresses_noflate_zlib(input in bounded_bytes()) {
        let compressed = noflate::zlib::compress(&input).expect("compress");
        let decompressed = flate2_zlib_decode(&compressed);
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn noflate_decompresses_flate2_gzip(input in bounded_bytes()) {
        let compressed = flate2_gzip_encode(&input);
        let decompressed = noflate::gzip::decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn flate2_decompresses_noflate_gzip(input in bounded_bytes()) {
        let compressed = noflate::gzip::compress(&input).expect("compress");
        let decompressed = flate2_gzip_decode(&compressed);
        prop_assert_eq!(decompressed, input);
    }

    #[test]
    fn adler32_matches_reference(input in bounded_bytes()) {
        let ours = noflate::adler32(&input);
        let reference = adler32::adler32(&input[..]).unwrap();
        prop_assert_eq!(ours, reference);
    }

    #[test]
    fn crc32_matches_reference(input in bounded_bytes()) {
        let ours = noflate::crc32(&input);
        let reference = crc32fast::hash(&input);
        prop_assert_eq!(ours, reference);
    }
}
