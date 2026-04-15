//! Compare noflate vs flate2 on encode / decode throughput and compression
//! ratio. Run with `cargo run --release --example bench`.
//!
//! Uses `std::time::Instant` rather than criterion so the noflate crate
//! itself stays dependency-free at runtime (flate2 is a dev-dep). libflate
//! was skipped because its current crates.io release depends on a yanked
//! core2 version; add it back via `path = "../libflate"` once that's
//! resolved.

use std::io::{Read, Write};
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
struct Sample {
    name: &'static str,
    bytes_out: usize,
    elapsed_ns: u128,
}

impl Sample {
    fn mb_per_sec(self, bytes: usize) -> f64 {
        if self.elapsed_ns == 0 {
            return f64::INFINITY;
        }
        (bytes as f64) / (self.elapsed_ns as f64) * 1_000.0
    }
}

fn time_once<F: FnOnce() -> Vec<u8>>(name: &'static str, _input_len: usize, f: F) -> Sample {
    let start = Instant::now();
    let out = f();
    let elapsed_ns = start.elapsed().as_nanos();
    Sample {
        name,
        bytes_out: out.len(),
        elapsed_ns,
    }
}

fn noflate_compress(input: &[u8]) -> Vec<u8> {
    noflate::compress(input).unwrap()
}

fn noflate_decompress(input: &[u8]) -> Vec<u8> {
    noflate::decompress(input).unwrap()
}

fn flate2_compress(input: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(input).unwrap();
    e.finish().unwrap()
}

fn flate2_decompress(input: &[u8]) -> Vec<u8> {
    let mut d = flate2::read::DeflateDecoder::new(input);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn bench_encode(input: &[u8], repeats: usize) {
    let mut samples = Vec::new();
    for _ in 0..repeats {
        samples.push(time_once("noflate", input.len(), || noflate_compress(input)));
    }
    for _ in 0..repeats {
        samples.push(time_once("flate2", input.len(), || flate2_compress(input)));
    }
    report_encode(input.len(), &samples);
}

fn bench_decode(input: &[u8], repeats: usize) {
    // Use flate2's compressed output as the canonical input so both
    // decoders process identical bytes.
    let compressed = flate2_compress(input);
    let mut samples = Vec::new();
    for _ in 0..repeats {
        samples.push(time_once("noflate", compressed.len(), || {
            noflate_decompress(&compressed)
        }));
    }
    for _ in 0..repeats {
        samples.push(time_once("flate2", compressed.len(), || {
            flate2_decompress(&compressed)
        }));
    }
    report_decode(compressed.len(), input.len(), &samples);
}

fn best<'a>(name: &'static str, samples: &'a [Sample]) -> Option<&'a Sample> {
    samples
        .iter()
        .filter(|s| s.name == name)
        .min_by_key(|s| s.elapsed_ns)
}

fn report_encode(input_len: usize, samples: &[Sample]) {
    println!("  encode  {input_len} bytes in");
    for name in ["noflate", "flate2"] {
        if let Some(best) = best(name, samples) {
            let ratio = best.bytes_out as f64 / input_len as f64;
            println!(
                "    {name:<10} {:>10} ns ({:>7.2} MB/s in)   out = {:>7} bytes, ratio = {:.4}",
                best.elapsed_ns,
                best.mb_per_sec(input_len),
                best.bytes_out,
                ratio,
            );
        }
    }
}

fn report_decode(compressed_len: usize, plaintext_len: usize, samples: &[Sample]) {
    println!("  decode  {compressed_len} bytes in -> {plaintext_len} bytes out");
    for name in ["noflate", "flate2"] {
        if let Some(best) = best(name, samples) {
            println!(
                "    {name:<10} {:>10} ns ({:>7.2} MB/s out)",
                best.elapsed_ns,
                best.mb_per_sec(plaintext_len),
            );
        }
    }
}

fn make_english_text(target_bytes: usize) -> Vec<u8> {
    let snippet = b"The quick brown fox jumps over the lazy dog. \
        Pack my box with five dozen liquor jugs. How vexingly quick daft \
        zebras jump! Sphinx of black quartz, judge my vow. The five boxing \
        wizards jump quickly. ";
    let mut out = Vec::with_capacity(target_bytes);
    while out.len() < target_bytes {
        out.extend_from_slice(snippet);
    }
    out.truncate(target_bytes);
    out
}

fn make_zeros(n: usize) -> Vec<u8> {
    vec![0u8; n]
}

fn make_random(n: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(n);
    out
}

fn run_case(name: &str, input: Vec<u8>, repeats: usize) {
    println!("=== {name} ===");
    bench_encode(&input, repeats);
    bench_decode(&input, repeats);
    println!();
}

fn main() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("english_1k", make_english_text(1_024)),
        ("english_64k", make_english_text(64 * 1024)),
        ("english_1m", make_english_text(1 * 1024 * 1024)),
        ("zeros_64k", make_zeros(64 * 1024)),
        ("random_64k", make_random(64 * 1024)),
    ];
    let repeats = std::env::var("BENCH_REPEATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    for (name, input) in cases {
        run_case(name, input, repeats);
    }
}
