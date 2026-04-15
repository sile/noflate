//! Canonical Huffman encoder and decoder plus length-limited code-length
//! construction (package-merge).
//!
//! Adapted from `nopng::deflate`; the algorithms are unchanged but types
//! are split out for reuse by the streaming codec.

use std::cmp;
use std::collections::BinaryHeap;

use crate::bit::{BitReader, BitWriter};
use crate::error::{Error, Result};
use crate::symbol::MAX_BITS;

fn reverse_bits(bits: u16, width: u8) -> u16 {
    let mut from = bits;
    let mut to = 0;
    for _ in 0..width {
        to <<= 1;
        to |= from & 1;
        from >>= 1;
    }
    to
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Code {
    width: u8,
    bits: u16,
}

impl Code {
    const EMPTY: Self = Self { width: 0, bits: 0 };
}

/// Encoder side of a canonical Huffman table built from per-symbol code
/// lengths.
#[derive(Debug, Clone)]
pub(crate) struct HuffmanEncoder {
    codes: Vec<Code>,
}

impl HuffmanEncoder {
    pub(crate) fn from_code_lengths(lengths: &[u8]) -> Result<Self> {
        let mut codes = vec![Code::EMPTY; lengths.len()];
        let mut symbols = lengths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 0)
            .map(|(symbol, width)| (symbol as u16, *width))
            .collect::<Vec<_>>();
        symbols.sort_by_key(|entry| entry.1);

        let mut code = 0u16;
        let mut previous_width = 0u8;
        for (symbol, width) in symbols {
            if width > MAX_BITS {
                return Err(Error::InvalidData(
                    "huffman code length exceeds 15 bits".into(),
                ));
            }
            code <<= width - previous_width;
            codes[symbol as usize] = Code {
                width,
                bits: reverse_bits(code, width),
            };
            code += 1;
            previous_width = width;
        }
        Ok(Self { codes })
    }

    pub(crate) fn encode(&self, writer: &mut BitWriter<'_>, symbol: u16) {
        let code = self.codes[symbol as usize];
        writer.write_bits(code.width, code.bits);
    }

    pub(crate) fn code_width(&self, symbol: u16) -> u8 {
        self.codes.get(symbol as usize).map_or(0, |code| code.width)
    }

    pub(crate) fn used_max_symbol(&self) -> Option<u16> {
        self.codes
            .iter()
            .rposition(|code| code.width > 0)
            .map(|index| index as u16)
    }
}

/// Decoder side: a flat lookup table with `2^max_bits` entries. Each entry
/// packs `(symbol << 5) | width`; `u16::MAX` is the empty sentinel.
#[derive(Debug)]
pub(crate) struct HuffmanDecoder {
    table: Vec<u16>,
    safely_peek_bits: u8,
    max_bits: u8,
}

impl HuffmanDecoder {
    pub(crate) fn from_code_lengths(
        lengths: &[u8],
        safely_peek_bits: Option<u8>,
        eob_symbol: Option<u16>,
    ) -> Result<Self> {
        let max_bits = lengths.iter().copied().max().unwrap_or(0);
        if max_bits == 0 {
            return Err(Error::InvalidData("huffman table is empty".into()));
        }
        if max_bits > MAX_BITS {
            return Err(Error::InvalidData(
                "huffman table uses too many bits".into(),
            ));
        }

        let table_len = 1usize << max_bits;
        let mut table = vec![u16::MAX; table_len];
        let mut entries = lengths
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, width)| *width > 0)
            .map(|(symbol, width)| (symbol as u16, width))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.1);

        let mut code = 0u16;
        let mut previous_width = 0u8;
        let mut actual_safely_peek = safely_peek_bits.unwrap_or(max_bits);

        for (symbol, width) in entries {
            code <<= width - previous_width;
            let reversed = reverse_bits(code, width);
            let value = (symbol << 5) | u16::from(width);
            let fill_count = 1usize << (max_bits - width);
            for padding in 0..fill_count {
                let index = ((padding as u16) << width | reversed) as usize;
                if table[index] != u16::MAX {
                    return Err(Error::InvalidData("conflicting huffman codes".into()));
                }
                table[index] = value;
            }
            if Some(symbol) == eob_symbol {
                actual_safely_peek = width;
            }
            code += 1;
            previous_width = width;
        }

        Ok(Self {
            table,
            safely_peek_bits: cmp::min(max_bits, actual_safely_peek.max(1)),
            max_bits,
        })
    }

    pub(crate) fn safely_peek_bits(&self) -> u8 {
        self.safely_peek_bits
    }

    pub(crate) fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        let mut peek_bits = self.safely_peek_bits;
        loop {
            let bits = reader.peek_bits(peek_bits)?;
            let value = self.table[bits as usize];
            let width = (value & 0b1_1111) as u8;
            if width <= peek_bits && value != u16::MAX {
                reader.skip_bits(width)?;
                return Ok(value >> 5);
            }
            if width > self.max_bits || value == u16::MAX {
                return Err(Error::InvalidData("invalid huffman coded stream".into()));
            }
            peek_bits = width;
        }
    }
}

/// Build code lengths from symbol frequencies capped at `max_bitwidth` bits.
pub(crate) fn length_limited_code_lengths(frequencies: &[usize], max_bitwidth: u8) -> Vec<u8> {
    let max_bitwidth = cmp::min(
        max_bitwidth,
        ordinary_huffman_optimal_max_bitwidth(frequencies),
    );
    package_merge_code_lengths(frequencies, max_bitwidth)
}

fn ordinary_huffman_optimal_max_bitwidth(frequencies: &[usize]) -> u8 {
    let mut heap = BinaryHeap::new();
    for &frequency in frequencies.iter().filter(|&&value| value > 0) {
        heap.push((-(frequency as isize), 0u8));
    }
    while heap.len() > 1 {
        let (weight1, width1) = heap.pop().expect("heap is non-empty");
        let (weight2, width2) = heap.pop().expect("heap has at least 2 entries");
        heap.push((weight1 + weight2, 1 + cmp::max(width1, width2)));
    }
    cmp::max(1, heap.pop().map_or(0, |(_, width)| width))
}

fn package_merge_code_lengths(frequencies: &[usize], max_bitwidth: u8) -> Vec<u8> {
    // Each node tracks a *list* of leaf symbols it subsumes (duplicates
    // allowed as the package-merge algorithm grows the list on every
    // merge). The final code width for symbol S equals the number of
    // times S appears across the final packaged list. This sparse
    // representation is much cheaper to clone than a dense per-symbol
    // counts vector when only a subset of the alphabet is in use.
    let symbol_count = frequencies.len();

    #[derive(Debug, Clone)]
    struct Node {
        symbols: Vec<u16>,
        weight: usize,
    }

    fn merge_nodes(left: Vec<Node>, right: Vec<Node>) -> Vec<Node> {
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let mut li = 0;
        let mut ri = 0;
        while li < left.len() && ri < right.len() {
            if left[li].weight < right[ri].weight {
                merged.push(left[li].clone());
                li += 1;
            } else {
                merged.push(right[ri].clone());
                ri += 1;
            }
        }
        merged.extend_from_slice(&left[li..]);
        merged.extend_from_slice(&right[ri..]);
        merged
    }

    fn package(nodes: &[Node]) -> Vec<Node> {
        if nodes.len() < 2 {
            return nodes.to_vec();
        }
        let new_len = nodes.len() / 2;
        let mut result = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let a = &nodes[i * 2];
            let b = &nodes[i * 2 + 1];
            let mut symbols = Vec::with_capacity(a.symbols.len() + b.symbols.len());
            symbols.extend_from_slice(&a.symbols);
            symbols.extend_from_slice(&b.symbols);
            result.push(Node {
                symbols,
                weight: a.weight + b.weight,
            });
        }
        result
    }

    let mut source: Vec<Node> = frequencies
        .iter()
        .enumerate()
        .filter(|(_, frequency)| **frequency > 0)
        .map(|(symbol, frequency)| Node {
            symbols: vec![symbol as u16],
            weight: *frequency,
        })
        .collect();
    source.sort_by_key(|node| node.weight);

    let weighted = (0..max_bitwidth.saturating_sub(1)).fold(source.clone(), |weighted, _| {
        merge_nodes(package(&weighted), source.clone())
    });

    let mut widths = vec![0u8; symbol_count];
    let packaged = package(&weighted);
    for node in &packaged {
        for &sym in &node.symbols {
            widths[sym as usize] += 1;
        }
    }
    widths
}

#[cfg(test)]
mod tests {
    use super::{HuffmanDecoder, HuffmanEncoder, length_limited_code_lengths};
    use crate::bit::{BitReader, BitWriter};
    use crate::symbol::{END_OF_BLOCK, fixed_literal_code_lengths};

    #[test]
    fn fixed_literal_table_roundtrip() {
        let lengths = fixed_literal_code_lengths();
        let enc = HuffmanEncoder::from_code_lengths(&lengths).unwrap();
        let dec = HuffmanDecoder::from_code_lengths(&lengths, None, Some(END_OF_BLOCK)).unwrap();

        let mut out = Vec::new();
        {
            let mut w = BitWriter::new(&mut out);
            for sym in 0u16..288 {
                if lengths[sym as usize] == 0 {
                    continue;
                }
                enc.encode(&mut w, sym);
            }
            w.finish();
        }

        let mut r = BitReader::new(&out);
        for sym in 0u16..288 {
            if lengths[sym as usize] == 0 {
                continue;
            }
            assert_eq!(dec.decode(&mut r).unwrap(), sym);
        }
    }

    #[test]
    fn empty_table_errors() {
        let lengths = [0u8; 5];
        assert!(HuffmanDecoder::from_code_lengths(&lengths, None, None).is_err());
    }

    #[test]
    fn length_limited_never_exceeds_max() {
        let freqs = [10usize, 3, 2, 1, 1, 1, 1, 1, 1, 1];
        let lengths = length_limited_code_lengths(&freqs, 4);
        for &l in &lengths {
            assert!(l <= 4);
        }
    }

    #[test]
    fn length_limited_all_used_symbols_get_codes() {
        let freqs = [5usize, 3, 2, 1];
        let lengths = length_limited_code_lengths(&freqs, 7);
        for (sym, &freq) in freqs.iter().enumerate() {
            if freq > 0 {
                assert!(lengths[sym] > 0, "symbol {sym} should have a code");
            }
        }
    }
}
