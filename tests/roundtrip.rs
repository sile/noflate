//! End-to-end `compress -> decompress` roundtrip tests covering all three
//! DEFLATE block kinds across a variety of inputs.

use noflate::{EncodeOptions, Encoder, decompress};

fn all_options() -> [EncodeOptions; 3] {
    [
        EncodeOptions::new(),
        EncodeOptions::new().fixed_huffman(),
        EncodeOptions::new().stored(),
    ]
}

fn compress_with(opts: EncodeOptions, input: &[u8]) -> Vec<u8> {
    let mut e = Encoder::with_options(opts);
    e.feed(input).unwrap();
    e.finish().unwrap();
    let out = e.output().to_vec();
    e.advance(out.len());
    assert!(e.is_finished());
    out
}

fn assert_roundtrip(input: &[u8]) {
    for opts in all_options() {
        let compressed = compress_with(opts.clone(), input);
        let decompressed = decompress(&compressed)
            .unwrap_or_else(|e| panic!("decompress failed for input len={}: {e}", input.len()));
        assert_eq!(decompressed, input, "mismatch for opts={opts:?}");
    }
}

#[test]
fn empty() {
    assert_roundtrip(b"");
}

#[test]
fn single_byte() {
    assert_roundtrip(b"X");
}

#[test]
fn short_text() {
    assert_roundtrip(b"Hello, DEFLATE!");
}

#[test]
fn below_min_match() {
    assert_roundtrip(b"ab");
}

#[test]
fn at_min_match() {
    assert_roundtrip(b"abc");
}

#[test]
fn all_zeros() {
    assert_roundtrip(&vec![0u8; 1024]);
}

#[test]
fn long_repeated_pattern() {
    let input: Vec<u8> = std::iter::repeat_n(b"banana ", 500).flatten().copied().collect();
    assert_roundtrip(&input);
}

#[test]
fn random_bytes_with_seeded_prng() {
    // Lightweight xorshift PRNG so we stay zero-dependency.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut input = vec![0u8; 4096];
    for byte in &mut input {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    assert_roundtrip(&input);
}

#[test]
fn english_paragraph() {
    let input = b"The quick brown fox jumps over the lazy dog. \
                  Pack my box with five dozen liquor jugs. \
                  How vexingly quick daft zebras jump!";
    assert_roundtrip(input);
}

#[test]
fn stored_multi_block_64k_plus() {
    let input = vec![b'x'; 70_000];
    let mut e = Encoder::with_options(EncodeOptions::new().stored());
    e.feed(&input).unwrap();
    e.finish().unwrap();
    let compressed = e.output().to_vec();
    e.advance(compressed.len());
    assert_eq!(decompress(&compressed).unwrap(), input);
}
