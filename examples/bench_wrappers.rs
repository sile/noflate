//! Measure noflate's zlib / gzip wrapper throughput.
//! Run with `cargo run --release --example bench_wrappers`.

use std::time::Instant;

#[derive(Debug, Clone, Copy)]
struct Sample {
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

fn time_once<F: FnOnce() -> Vec<u8>>(f: F) -> Sample {
    let start = Instant::now();
    let out = f();
    Sample {
        bytes_out: out.len(),
        elapsed_ns: start.elapsed().as_nanos(),
    }
}

fn best_of<F: Fn() -> Vec<u8>>(repeats: usize, f: F) -> Sample {
    let mut best = Sample {
        bytes_out: 0,
        elapsed_ns: u128::MAX,
    };
    for _ in 0..repeats {
        let sample = time_once(&f);
        if sample.elapsed_ns < best.elapsed_ns {
            best = sample;
        }
    }
    best
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

fn run_codec(
    label: &str,
    input: &[u8],
    repeats: usize,
    compress: impl Fn(&[u8]) -> Vec<u8>,
    decompress: impl Fn(&[u8]) -> Vec<u8>,
    decode_streaming: impl Fn(&[u8]) -> usize,
) {
    let encode = best_of(repeats, || compress(input));
    let compressed = compress(input);
    let decode = best_of(repeats, || decompress(&compressed));
    let decode_stream = best_of(repeats, || {
        let n = decode_streaming(&compressed);
        vec![0; n]
    });
    println!("  {label}");
    println!(
        "    encode {:>10} ns ({:>7.2} MB/s in)   out = {:>7} bytes",
        encode.elapsed_ns,
        encode.mb_per_sec(input.len()),
        encode.bytes_out,
    );
    println!(
        "    decode {:>10} ns ({:>7.2} MB/s out)",
        decode.elapsed_ns,
        decode.mb_per_sec(input.len()),
    );
    println!(
        "    stream {:>10} ns ({:>7.2} MB/s out)   advance=1024 bytes",
        decode_stream.elapsed_ns,
        decode_stream.mb_per_sec(input.len()),
    );
}

fn decode_zlib_streaming(input: &[u8]) -> usize {
    let mut decoder = noflate::zlib::Decoder::new();
    decoder.feed(input).unwrap();
    let mut total = 0usize;
    while !decoder.output().is_empty() {
        let take = decoder.output().len().min(1024);
        total += take;
        decoder.advance(take);
    }
    assert!(decoder.is_finished());
    total
}

fn decode_gzip_streaming(input: &[u8]) -> usize {
    let mut decoder = noflate::gzip::Decoder::new();
    decoder.feed(input).unwrap();
    let mut total = 0usize;
    while !decoder.output().is_empty() {
        let take = decoder.output().len().min(1024);
        total += take;
        decoder.advance(take);
    }
    assert!(decoder.is_finished());
    total
}

fn main() {
    let repeats = std::env::var("BENCH_REPEATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    for (name, input) in [
        ("english_64k", make_english_text(64 * 1024)),
        ("english_1m", make_english_text(1024 * 1024)),
        ("random_64k", make_random(64 * 1024)),
    ] {
        println!("=== {name} ===");
        run_codec(
            "zlib",
            &input,
            repeats,
            |bytes| noflate::zlib::compress(bytes).unwrap(),
            |bytes| noflate::zlib::decompress(bytes).unwrap(),
            decode_zlib_streaming,
        );
        run_codec(
            "gzip",
            &input,
            repeats,
            |bytes| noflate::gzip::compress(bytes).unwrap(),
            |bytes| noflate::gzip::decompress(bytes).unwrap(),
            decode_gzip_streaming,
        );
        println!();
    }
}
