//! Property-based tests for noflate, driven by noprop.
//!
//! Coverage mirrors the previous proptest suite:
//!
//! - Self-roundtrip for DEFLATE / ZLIB / GZIP across the three
//!   encoder block kinds (stored / fixed Huffman / dynamic Huffman).
//! - Chunked feed equivalence for both decoder and encoder.
//! - Binary interoperability with `flate2` in both directions.
//! - Checksum agreement with the `adler32` and `crc32fast` crates.
//!
//! Plus noprop-specific additions:
//!
//! - **Stateful encoder command loop** — drives the encoder through a
//!   random sequence of `feed` / `sync_flush` / `reset_history` /
//!   `finish` and asserts the full output round-trips through the
//!   decoder. proptest models this as a single generator call, which
//!   cannot express the interactive shape.
//! - **Stateful decoder loop** — interleaves random feed chunks with
//!   random partial output drains and asserts that every observation
//!   is a prefix of the true decoded data, that the decoder finishes
//!   exactly when the whole stream has been fed, and that feeding
//!   after the end of the stream is rejected.
//! - **Streaming container encoders** — random chunked feeds into the
//!   gzip / zlib streaming encoders round-trip, exercising the
//!   header / trailer emission paths that the one-shot `compress`
//!   functions (which use `buffer_all_input`) do not.
//! - **Streaming checksum equivalence** — incremental `Adler32` /
//!   `Crc32` updates over random chunk boundaries agree with the
//!   one-shot checksum functions.
//! - **flate2 interop at every compression level** — the decoder must
//!   accept real-world streams from levels 0..=9 (stored through
//!   dynamic Huffman), not just flate2's default level.
//! - **Feedback-guided input diversity** — reports semantic buckets
//!   (input length band, distinct byte count) so `run_feedback_guided`
//!   concentrates cases on the corners uniform sampling would only
//!   stumble on (long inputs, all-zero inputs, all-256-byte inputs).
//!
//! Inputs are capped at 32 KiB (proptest's suite used 64 KiB); the
//! properties don't depend on very large inputs and the smaller cap
//! keeps noprop runs snappy. Input lengths are drawn with boundary
//! sampling so the empty, singleton, and maximum classes are exercised
//! with meaningful probability instead of ~1/32769 each.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
use noflate::deflate::{EncodeOptions, Encoder};
use noprop::TestCaseContext;

// --- Runner config ---------------------------------------------------

const SEED: u64 = 0xDEAD_BEEF_1234_5678;
const CASES: usize = 64;
const MAX_INPUT: usize = 32 * 1024;

fn run<F>(f: F) -> noprop::TestResult
where
    F: Fn(&mut TestCaseContext) -> noprop::TestResult,
{
    noprop::Runner::new(SEED).run(CASES, f)?;
    Ok(())
}

fn run_feedback<F>(cases: usize, f: F) -> noprop::TestResult
where
    F: Fn(&mut TestCaseContext) -> noprop::TestResult,
{
    noprop::Runner::new(SEED).run_feedback_guided(cases, f)?;
    Ok(())
}

// --- Input generators ------------------------------------------------

fn sample_input(ctx: &mut TestCaseContext) -> Vec<u8> {
    // Boundary sampling gives the empty, singleton, and maximum classes
    // meaningful probability instead of the ~1/32769 a uniform draw over
    // 0..=MAX_INPUT would give them.
    let len = noprop::sample_with_boundaries(
        ctx,
        &[0usize, 1, MAX_INPUT],
        noprop::Ratio::one_nth(4),
        |ctx| noprop::sample_usize_in(ctx, 0..=MAX_INPUT),
    );
    noprop::sample_bytes_vec(ctx, len)
}

#[test]
fn sample_input_reaches_boundary_classes() -> noprop::TestResult {
    // Gate on the generator itself: the empty, singleton, and maximum
    // classes must each be observed within the case budget so the
    // roundtrip properties never pass vacuously over them. If someone
    // drops the boundary sampling from `sample_input`, this test fails.
    use std::cell::Cell;
    let hit_empty = Cell::new(false);
    let hit_singleton = Cell::new(false);
    let hit_max = Cell::new(false);
    noprop::Runner::new(SEED).run(CASES, |ctx| {
        let input = sample_input(ctx);
        hit_empty.set(hit_empty.get() || input.is_empty());
        hit_singleton.set(hit_singleton.get() || input.len() == 1);
        hit_max.set(hit_max.get() || input.len() == MAX_INPUT);
        Ok(())
    })?;
    assert!(hit_empty.get(), "no case drew the empty input class");
    assert!(
        hit_singleton.get(),
        "no case drew the singleton input class"
    );
    assert!(hit_max.get(), "no case drew the maximum input class");
    Ok(())
}

/// Chunk-size sequence: 1..=64 chunks, each 1..=128 bytes.
fn sample_chunks(ctx: &mut TestCaseContext) -> Vec<usize> {
    let n = noprop::sample_usize_in(ctx, 1..=64);
    (0..n)
        .map(|_| noprop::sample_usize_in(ctx, 1..=128))
        .collect()
}

// --- flate2 reference helpers ---------------------------------------

fn flate2_deflate_at_level(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_inflate(data: &[u8]) -> Vec<u8> {
    let mut d = DeflateDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn flate2_zlib_encode_at_level(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_zlib_decode(data: &[u8]) -> Vec<u8> {
    let mut d = ZlibDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn flate2_gzip_encode_at_level(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn flate2_gzip_decode(data: &[u8]) -> Vec<u8> {
    let mut d = GzDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

// --- noflate encode / decode helpers ---------------------------------

fn compress_with(opts: EncodeOptions, input: &[u8]) -> Vec<u8> {
    let mut e = Encoder::with_options(opts);
    e.feed(input).expect("encoder feed");
    e.finish().expect("encoder finish");
    let out = e.output().to_vec();
    e.advance(out.len());
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

fn chunked_gzip_encoder_output(input: &[u8], chunks: &[usize]) -> Vec<u8> {
    let mut e = noflate::gzip::Encoder::new();
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

fn chunked_zlib_encoder_output(input: &[u8], chunks: &[usize]) -> Vec<u8> {
    let mut e = noflate::zlib::Encoder::new();
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

// --- Roundtrip tests -------------------------------------------------

#[test]
fn deflate_dynamic_roundtrip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = compress_with(EncodeOptions::new(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn deflate_fixed_roundtrip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = compress_with(EncodeOptions::new().fixed_huffman(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn deflate_stored_roundtrip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = compress_with(EncodeOptions::new().stored(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn zlib_roundtrip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::zlib::compress(&input).expect("compress");
        let decompressed = noflate::zlib::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn gzip_roundtrip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::gzip::compress(&input).expect("compress");
        let decompressed = noflate::gzip::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn decoder_chunked_feed_matches_whole() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let compressed = noflate::deflate::compress(&input).expect("compress");
        let out = chunked_decoder_output(&compressed, &chunks);
        assert_eq!(out, input);
        Ok(())
    })
}

#[test]
fn encoder_chunked_feed_roundtrips() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let compressed = chunked_encoder_output(&input, &chunks);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn gzip_encoder_chunked_feed_roundtrips() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let compressed = chunked_gzip_encoder_output(&input, &chunks);
        let decompressed = noflate::gzip::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn zlib_encoder_chunked_feed_roundtrips() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let compressed = chunked_zlib_encoder_output(&input, &chunks);
        let decompressed = noflate::zlib::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

// --- Interop with flate2 --------------------------------------------

#[test]
fn noflate_decompresses_flate2_deflate_all_levels() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let level = noprop::sample_usize_in(ctx, 0..=9) as u32;
        let compressed = flate2_deflate_at_level(&input, level);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn flate2_decompresses_noflate_deflate() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::deflate::compress(&input).expect("compress");
        let decompressed = flate2_inflate(&compressed);
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn noflate_decompresses_flate2_zlib_all_levels() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let level = noprop::sample_usize_in(ctx, 0..=9) as u32;
        let compressed = flate2_zlib_encode_at_level(&input, level);
        let decompressed = noflate::zlib::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn flate2_decompresses_noflate_zlib() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::zlib::compress(&input).expect("compress");
        let decompressed = flate2_zlib_decode(&compressed);
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn noflate_decompresses_flate2_gzip_all_levels() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let level = noprop::sample_usize_in(ctx, 0..=9) as u32;
        let compressed = flate2_gzip_encode_at_level(&input, level);
        let decompressed = noflate::gzip::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}

#[test]
fn flate2_decompresses_noflate_gzip() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::gzip::compress(&input).expect("compress");
        let decompressed = flate2_gzip_decode(&compressed);
        assert_eq!(decompressed, input);
        Ok(())
    })
}

// --- Checksum agreement ---------------------------------------------

#[test]
fn adler32_matches_reference() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let ours = noflate::zlib::adler32(&input);
        let reference = adler32::adler32(&input[..]).unwrap();
        assert_eq!(ours, reference);
        Ok(())
    })
}

#[test]
fn crc32_matches_reference() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let ours = noflate::gzip::crc32(&input);
        let reference = crc32fast::hash(&input);
        assert_eq!(ours, reference);
        Ok(())
    })
}

#[test]
fn adler32_streaming_matches_one_shot() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let mut incremental = noflate::zlib::Adler32::new();
        let mut offset = 0;
        for &chunk in &chunks {
            if offset >= input.len() {
                break;
            }
            let end = (offset + chunk).min(input.len());
            incremental.update(&input[offset..end]);
            offset = end;
        }
        if offset < input.len() {
            incremental.update(&input[offset..]);
        }
        assert_eq!(incremental.value(), noflate::zlib::adler32(&input));
        Ok(())
    })
}

#[test]
fn crc32_streaming_matches_one_shot() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let chunks = sample_chunks(ctx);
        let mut incremental = noflate::gzip::Crc32::new();
        let mut offset = 0;
        for &chunk in &chunks {
            if offset >= input.len() {
                break;
            }
            let end = (offset + chunk).min(input.len());
            incremental.update(&input[offset..end]);
            offset = end;
        }
        if offset < input.len() {
            incremental.update(&input[offset..]);
        }
        assert_eq!(incremental.value(), noflate::gzip::crc32(&input));
        Ok(())
    })
}

// --- Stateful: encoder command loop ---------------------------------
//
// Drives the encoder through a random sequence of feed / sync_flush /
// reset_history / finish and asserts the full concatenated output
// round-trips through the decoder. The model is the flat concatenation
// of every `feed` payload up to `finish`; sync_flush and
// reset_history do not change the model. proptest's single-generator
// shape cannot express this interactive sequence cleanly.

/// One command in the encoder command loop.
#[derive(Debug, Clone)]
enum Cmd {
    Feed(Vec<u8>),
    SyncFlush,
    ResetHistory,
}

fn sample_cmd(ctx: &mut TestCaseContext) -> Cmd {
    // 60% Feed, 25% SyncFlush, 15% ResetHistory
    match noprop::sample_weighted_index(ctx, &[60, 25, 15]) {
        0 => {
            let len = noprop::sample_usize_in(ctx, 0..=256);
            Cmd::Feed(noprop::sample_bytes_vec(ctx, len))
        }
        1 => Cmd::SyncFlush,
        _ => Cmd::ResetHistory,
    }
}

#[test]
fn stateful_encoder_command_loop_roundtrips() -> noprop::TestResult {
    noprop::Runner::new(SEED).run(32, |ctx| {
        let n_cmds = noprop::sample_usize_in(ctx, 0..=32);
        let mut encoder = Encoder::new();
        let mut expected: Vec<u8> = Vec::new();
        for _ in 0..n_cmds {
            match sample_cmd(ctx) {
                Cmd::Feed(bytes) => {
                    encoder
                        .feed(&bytes)
                        .expect("feed must succeed before finish");
                    expected.extend_from_slice(&bytes);
                }
                Cmd::SyncFlush => {
                    encoder
                        .sync_flush()
                        .expect("sync_flush must succeed before finish");
                }
                Cmd::ResetHistory => {
                    encoder.reset_history();
                }
            }
        }
        encoder.finish().expect("finish");
        let compressed = encoder.output().to_vec();
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(
            decompressed, expected,
            "command loop must round-trip to the concatenated feed payloads"
        );
        Ok(())
    })?;
    Ok(())
}

// --- Stateful: decoder feed / drain loop ----------------------------
//
// Interleaves random feed chunks with random partial output drains and
// asserts three invariants: every observation is a prefix of the true
// decoded data (output must be stable across partial consumption),
// the decoder finishes exactly once the whole stream has been fed, and
// feeding bytes after the end of the stream is rejected.

#[test]
fn stateful_decoder_feed_and_drain_roundtrips() -> noprop::TestResult {
    run(|ctx| {
        let input = sample_input(ctx);
        let compressed = noflate::deflate::compress(&input).expect("compress");
        let chunks = sample_chunks(ctx);
        let mut decoder = noflate::deflate::Decoder::new();
        let mut decoded: Vec<u8> = Vec::new();
        let mut fed = 0usize;
        for &chunk in &chunks {
            if fed >= compressed.len() {
                break;
            }
            let end = (fed + chunk).min(compressed.len());
            decoder.feed(&compressed[fed..end]).expect("feed");
            fed = end;
            // Drain a random prefix of the pending output; the drained
            // bytes must always be a prefix of the true decoded data.
            let drain = noprop::sample_usize_in(ctx, 0..=decoder.output().len());
            decoded.extend_from_slice(&decoder.output()[..drain]);
            decoder.advance(drain);
            assert_eq!(
                decoded,
                input[..decoded.len()],
                "partial drains must yield a prefix of the decoded data"
            );
        }
        if fed < compressed.len() {
            decoder.feed(&compressed[fed..]).expect("tail feed");
        }
        let drain = decoder.output().len();
        decoded.extend_from_slice(&decoder.output()[..drain]);
        decoder.advance(drain);
        assert!(
            decoder.is_finished(),
            "decoder must finish once the whole stream has been fed"
        );
        assert_eq!(decoded, input);
        assert!(
            decoder.feed(&[0u8]).is_err(),
            "feeding after the end of the stream must be rejected"
        );
        Ok(())
    })
}

// --- Feedback-guided: input diversity --------------------------------
//
// Reports two semantic buckets to `run_feedback_guided` so the search
// concentrates cases on the corners uniform sampling would only
// stumble on: input length band and distinct byte count. The property
// remains a straight roundtrip check.

fn distinct_byte_count(bytes: &[u8]) -> usize {
    let mut seen = [false; 256];
    let mut count = 0;
    for &b in bytes {
        if !seen[b as usize] {
            seen[b as usize] = true;
            count += 1;
        }
    }
    count
}

#[test]
fn feedback_guided_deflate_dynamic_roundtrip() -> noprop::TestResult {
    run_feedback(64, |ctx| {
        let input = sample_input(ctx);
        // Length band: 0 / 1..=1024 / 1025..=8192 / 8193..=32768.
        let len_band = match input.len() {
            0 => 0u64,
            1..=1024 => 1,
            1025..=8192 => 2,
            _ => 3,
        };
        ctx.bucket("input_len_band", len_band);
        // Distinct byte count bucketed as 0 / 1 / 2..=15 / 16..=127 /
        // 128..=255 / 256 (all bytes present).
        let distinct = distinct_byte_count(&input);
        let distinct_band = match distinct {
            0 => 0u64,
            1 => 1,
            2..=15 => 2,
            16..=127 => 3,
            128..=255 => 4,
            _ => 5,
        };
        ctx.bucket("distinct_byte_count", distinct_band);
        let compressed = compress_with(EncodeOptions::new(), &input);
        let decompressed = noflate::deflate::decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
        Ok(())
    })
}
