//! Adler-32 checksum (RFC 1950).
//!
//! Each [`Adler32::update`] call runs 16 additions per loop and only takes
//! the modulus once per 5552-byte chunk (the maximum `n` for which both
//! accumulators are guaranteed not to overflow a `u32`).

const BASE: u32 = 65_521;
const NMAX: usize = 5_552;

/// Streaming Adler-32 checksum.
#[derive(Debug, Clone)]
pub struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    /// Create a new Adler-32 accumulator with the RFC 1950 initial state.
    pub fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    /// Feed more bytes.
    pub fn update(&mut self, data: &[u8]) {
        let mut a = self.a;
        let mut b = self.b;
        let mut offset = 0;
        while offset < data.len() {
            let chunk_end = (offset + NMAX).min(data.len());
            let chunk = &data[offset..chunk_end];
            // Inner loop: unroll 16 at a time for speed.
            let mut i = 0;
            while i + 16 <= chunk.len() {
                for j in 0..16 {
                    a = a.wrapping_add(chunk[i + j] as u32);
                    b = b.wrapping_add(a);
                }
                i += 16;
            }
            while i < chunk.len() {
                a = a.wrapping_add(chunk[i] as u32);
                b = b.wrapping_add(a);
                i += 1;
            }
            a %= BASE;
            b %= BASE;
            offset = chunk_end;
        }
        self.a = a;
        self.b = b;
    }

    /// Return the current 32-bit Adler-32 value
    /// `(b << 16) | a` in RFC 1950 byte order.
    pub fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// One-shot convenience: compute Adler-32 of a full byte slice.
pub fn adler32(data: &[u8]) -> u32 {
    let mut a = Adler32::new();
    a.update(data);
    a.value()
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::Adler32;

    fn checksum(data: &[u8]) -> u32 {
        super::adler32(data)
    }

    #[test]
    fn known_vectors() {
        assert_eq!(checksum(b""), 1);
        assert_eq!(checksum(b"a"), 0x0062_0062);
        // "Wikipedia" per Wikipedia's Adler-32 article.
        assert_eq!(checksum(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn streaming_equals_one_shot() {
        let input: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
        let one_shot = checksum(&input);
        let mut a = Adler32::new();
        for chunk in input.chunks(37) {
            a.update(chunk);
        }
        assert_eq!(a.value(), one_shot);
    }

    #[test]
    fn matches_reference_crate() {
        let input: Vec<u8> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
        let ours = checksum(&input);
        let reference = adler32::adler32(&input[..]).unwrap();
        assert_eq!(ours, reference);
    }
}
