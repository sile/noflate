#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Decode arbitrary bytes; must not panic. On success, recompressing
    // and decompressing must round-trip back to the original decoded
    // payload.
    if let Ok(decoded) = noflate::decompress(data)
        && let Ok(recompressed) = noflate::compress(&decoded)
    {
        let redecoded = noflate::decompress(&recompressed).expect("self-roundtrip");
        assert_eq!(redecoded, decoded);
    }
});
