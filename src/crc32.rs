//! CRC-32 (IEEE 802.3, used by gzip) checksum.
//!
//! Uses a "slice-by-16" lookup strategy: a static table of 16 × 256 u32
//! entries (16 KiB, built at compile time via `const fn`) lets
//! [`Crc32::update`] process 16 input bytes per iteration with 16
//! lookups and XORs. This is stable-Rust safe portable pseudo-SIMD; it
//! won't match CLMUL hardware CRC but clears several GB/s on modern CPUs.

/// Reflected CRC-32 polynomial (standard IEEE 802.3 CRC).
const POLYNOMIAL: u32 = 0xEDB8_8320;

const SLICE: usize = 16;

const TABLE: [[u32; 256]; SLICE] = make_table();

const fn make_table() -> [[u32; 256]; SLICE] {
    let mut table = [[0u32; 256]; SLICE];

    // Classic reflected CRC-32 byte table.
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ POLYNOMIAL
            } else {
                c >> 1
            };
            j += 1;
        }
        table[0][i] = c;
        i += 1;
    }

    // Derived tables: table[k][b] == CRC of [b, 0, 0, ..., 0] (k zeros).
    let mut k = 1;
    while k < SLICE {
        let mut i = 0;
        while i < 256 {
            let prev = table[k - 1][i];
            table[k][i] = (prev >> 8) ^ table[0][(prev & 0xFF) as usize];
            i += 1;
        }
        k += 1;
    }

    table
}

/// Streaming CRC-32 accumulator.
#[derive(Debug, Clone)]
pub struct Crc32 {
    // Stored *inverted* (initial 0xFFFFFFFF) so the inner loop does not
    // re-invert on each `update` call. `value` inverts on the way out.
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// New accumulator with the RFC 1952 initial state.
    pub fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Feed more bytes.
    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        let mut i = 0;
        while i + SLICE <= data.len() {
            // Treat the 16-byte chunk as four little-endian u32s. The
            // first is XORed with the current CRC; the others are used
            // as-is.
            let w0 = u32::from_le_bytes(data[i..i + 4].try_into().expect("4 bytes")) ^ crc;
            let w1 = u32::from_le_bytes(data[i + 4..i + 8].try_into().expect("4 bytes"));
            let w2 = u32::from_le_bytes(data[i + 8..i + 12].try_into().expect("4 bytes"));
            let w3 = u32::from_le_bytes(data[i + 12..i + 16].try_into().expect("4 bytes"));
            // Slice-by-16: table[15-k] indexes "byte k from the start".
            crc = TABLE[15][(w0 & 0xFF) as usize]
                ^ TABLE[14][((w0 >> 8) & 0xFF) as usize]
                ^ TABLE[13][((w0 >> 16) & 0xFF) as usize]
                ^ TABLE[12][(w0 >> 24) as usize]
                ^ TABLE[11][(w1 & 0xFF) as usize]
                ^ TABLE[10][((w1 >> 8) & 0xFF) as usize]
                ^ TABLE[9][((w1 >> 16) & 0xFF) as usize]
                ^ TABLE[8][(w1 >> 24) as usize]
                ^ TABLE[7][(w2 & 0xFF) as usize]
                ^ TABLE[6][((w2 >> 8) & 0xFF) as usize]
                ^ TABLE[5][((w2 >> 16) & 0xFF) as usize]
                ^ TABLE[4][(w2 >> 24) as usize]
                ^ TABLE[3][(w3 & 0xFF) as usize]
                ^ TABLE[2][((w3 >> 8) & 0xFF) as usize]
                ^ TABLE[1][((w3 >> 16) & 0xFF) as usize]
                ^ TABLE[0][(w3 >> 24) as usize];
            i += SLICE;
        }
        while i < data.len() {
            crc = (crc >> 8) ^ TABLE[0][((crc ^ u32::from(data[i])) & 0xFF) as usize];
            i += 1;
        }
        self.state = crc;
    }

    /// Finalise and return the 32-bit CRC-32 value.
    pub fn value(&self) -> u32 {
        !self.state
    }
}

/// One-shot convenience: compute CRC-32 of a full byte slice.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.value()
}

#[cfg(test)]
mod tests {
    use super::Crc32;

    fn checksum(data: &[u8]) -> u32 {
        super::crc32(data)
    }

    #[test]
    fn known_vectors() {
        // Per the POSIX cksum / cksum -a crc32 standard vectors.
        assert_eq!(checksum(b""), 0);
        assert_eq!(checksum(b"a"), 0xE8B7_BE43);
        assert_eq!(
            checksum(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn streaming_equals_one_shot() {
        let input: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
        let one_shot = checksum(&input);
        let mut c = Crc32::new();
        for chunk in input.chunks(37) {
            c.update(chunk);
        }
        assert_eq!(c.value(), one_shot);
    }

    #[test]
    fn matches_reference_crate() {
        let input: Vec<u8> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
        let ours = checksum(&input);
        let reference = crc32fast::hash(&input);
        assert_eq!(ours, reference);
    }

    #[test]
    fn slice_by_8_matches_byte_at_a_time_for_various_lengths() {
        let data: Vec<u8> = (0..100).collect();
        for len in 0..=data.len() {
            let mut slow = Crc32::new();
            for &b in &data[..len] {
                slow.update(std::slice::from_ref(&b));
            }
            let fast = checksum(&data[..len]);
            assert_eq!(slow.value(), fast, "len={len}");
        }
    }
}
