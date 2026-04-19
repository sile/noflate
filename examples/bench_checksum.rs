//! Compare noflate's checksum implementations against crc32fast and the
//! adler32 crate. Run with `cargo run --release --example bench_checksum`.

use std::io::Read;
use std::time::Instant;

fn time_checksum(_name: &'static str, _len: usize, f: impl Fn() -> u32) -> (u128, u32) {
    let start = Instant::now();
    let value = f();
    let elapsed_ns = start.elapsed().as_nanos();
    (elapsed_ns, value)
}

fn mb_per_sec(bytes: usize, elapsed_ns: u128) -> f64 {
    (bytes as f64) / (elapsed_ns as f64) * 1_000.0
}

fn bench_one(name: &'static str, bytes: usize, repeats: usize) {
    let input: Vec<u8> = (0..bytes).map(|i| (i * 31) as u8).collect();

    let mut noflate_adler_best = u128::MAX;
    let mut crate_adler_best = u128::MAX;
    let mut noflate_crc_best = u128::MAX;
    let mut crate_crc_best = u128::MAX;

    for _ in 0..repeats {
        let (n, _) = time_checksum("noflate adler32", bytes, || noflate::zlib::adler32(&input));
        noflate_adler_best = noflate_adler_best.min(n);
    }
    for _ in 0..repeats {
        let (n, _) = time_checksum("adler32 crate", bytes, || {
            adler32::adler32(&mut input.as_slice().take(input.len() as u64)).unwrap()
        });
        crate_adler_best = crate_adler_best.min(n);
    }
    for _ in 0..repeats {
        let (n, _) = time_checksum("noflate crc32", bytes, || noflate::gzip::crc32(&input));
        noflate_crc_best = noflate_crc_best.min(n);
    }
    for _ in 0..repeats {
        let (n, _) = time_checksum("crc32fast", bytes, || crc32fast::hash(&input));
        crate_crc_best = crate_crc_best.min(n);
    }

    println!("=== {name} ({bytes} bytes) ===");
    println!(
        "  adler32  noflate={:>7.2} MB/s   crate={:>7.2} MB/s  (ratio {:.2}x)",
        mb_per_sec(bytes, noflate_adler_best),
        mb_per_sec(bytes, crate_adler_best),
        mb_per_sec(bytes, noflate_adler_best) / mb_per_sec(bytes, crate_adler_best),
    );
    println!(
        "  crc32    noflate={:>7.2} MB/s   fast ={:>7.2} MB/s  (ratio {:.2}x)",
        mb_per_sec(bytes, noflate_crc_best),
        mb_per_sec(bytes, crate_crc_best),
        mb_per_sec(bytes, noflate_crc_best) / mb_per_sec(bytes, crate_crc_best),
    );
    println!();
}

fn main() {
    let repeats = std::env::var("BENCH_REPEATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    for (name, bytes) in [
        ("small_256", 256),
        ("medium_64k", 64 * 1024),
        ("large_1m", 1024 * 1024),
        ("huge_16m", 16 * 1024 * 1024),
    ] {
        bench_one(name, bytes, repeats);
    }
}
