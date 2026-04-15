#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(decoded) = noflate::zlib::decompress(data) {
        if let Ok(recompressed) = noflate::zlib::compress(&decoded) {
            let redecoded = noflate::zlib::decompress(&recompressed).expect("self-roundtrip");
            assert_eq!(redecoded, decoded);
        }
    }
});
