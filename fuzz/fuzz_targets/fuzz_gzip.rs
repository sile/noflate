#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(decoded) = noflate::gzip::decompress(data)
        && let Ok(recompressed) = noflate::gzip::compress(&decoded)
    {
        let redecoded = noflate::gzip::decompress(&recompressed).expect("self-roundtrip");
        assert_eq!(redecoded, decoded);
    }
});
