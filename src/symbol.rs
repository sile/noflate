//! Static RFC 1951 symbol tables and helpers.

/// Maximum code width allowed for any DEFLATE Huffman code.
pub(crate) const MAX_BITS: u8 = 15;

/// End-of-block symbol in the literal/length alphabet.
pub(crate) const END_OF_BLOCK: u16 = 256;

/// Maximum back-reference distance.
pub(crate) const WINDOW_SIZE: usize = 32_768;

/// Minimum LZ77 match length.
pub(crate) const MIN_MATCH: usize = 3;

/// Maximum LZ77 match length.
pub(crate) const MAX_MATCH: usize = 258;

/// Maximum payload size of a stored (BTYPE=00) block.
pub(crate) const MAX_STORED_BLOCK: usize = 0xFFFF;

/// The code-length alphabet's symbols appear in this order when transmitted.
pub(crate) const BITWIDTH_CODE_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Length codes: `(base_length, extra_bits)` for symbols 257..=285.
pub(crate) const LENGTH_TABLE: [(u16, u8); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];

/// Distance codes: `(base_distance, extra_bits)` for symbols 0..=29.
pub(crate) const DISTANCE_TABLE: [(u16, u8); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12_289, 12),
    (16_385, 13),
    (24_577, 13),
];

/// Result of encoding a literal/length or distance value into a Huffman
/// alphabet symbol plus optional extra bits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncodedSymbol {
    pub(crate) code: u16,
    pub(crate) extra: Option<(u8, u16)>,
}

pub(crate) fn length_to_symbol(length: u16) -> EncodedSymbol {
    // Iterate from the high end; the first entry whose base is <= length
    // is the correct code. This naturally handles the special case of
    // length=258 (code 285) whose single-value range would otherwise
    // overlap with the 227..=257 range of code 284.
    for (index, &(base, extra_bits)) in LENGTH_TABLE.iter().enumerate().rev() {
        if length >= base {
            return EncodedSymbol {
                code: 257 + index as u16,
                extra: (extra_bits > 0).then_some((extra_bits, length - base)),
            };
        }
    }
    unreachable!("invalid length: {length}")
}

pub(crate) fn distance_to_symbol(distance: u16) -> EncodedSymbol {
    for (index, &(base, extra_bits)) in DISTANCE_TABLE.iter().enumerate().rev() {
        if distance >= base {
            return EncodedSymbol {
                code: index as u16,
                extra: (extra_bits > 0).then_some((extra_bits, distance - base)),
            };
        }
    }
    unreachable!("invalid distance: {distance}")
}

/// Fixed-block literal/length code lengths per RFC 1951 §3.2.6.
pub(crate) fn fixed_literal_code_lengths() -> [u8; 288] {
    let mut lengths = [0u8; 288];
    for (index, length) in lengths.iter_mut().enumerate() {
        *length = match index {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    lengths
}

/// Fixed-block distance code lengths per RFC 1951 §3.2.6 (all 5).
pub(crate) fn fixed_distance_code_lengths() -> [u8; 30] {
    [5u8; 30]
}

#[cfg(test)]
mod tests {
    use super::{distance_to_symbol, length_to_symbol};

    #[test]
    fn length_corner_cases() {
        let s = length_to_symbol(3);
        assert_eq!(s.code, 257);
        assert_eq!(s.extra, None);

        let s = length_to_symbol(10);
        assert_eq!(s.code, 264);
        assert_eq!(s.extra, None);

        let s = length_to_symbol(11);
        assert_eq!(s.code, 265);
        assert_eq!(s.extra, Some((1, 0)));

        let s = length_to_symbol(12);
        assert_eq!(s.code, 265);
        assert_eq!(s.extra, Some((1, 1)));

        let s = length_to_symbol(258);
        assert_eq!(s.code, 285);
        assert_eq!(s.extra, None);
    }

    #[test]
    fn distance_corner_cases() {
        let s = distance_to_symbol(1);
        assert_eq!(s.code, 0);
        assert_eq!(s.extra, None);

        let s = distance_to_symbol(5);
        assert_eq!(s.code, 4);
        assert_eq!(s.extra, Some((1, 0)));

        let s = distance_to_symbol(32768);
        assert_eq!(s.code, 29);
        assert_eq!(s.extra, Some((13, 32768 - 24577)));
    }
}
