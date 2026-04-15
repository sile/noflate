//! One-shot LZ77 match finder.
//!
//! The streaming encoder buffers its input and, on `finish()` or when a
//! block fills up, calls [`lz77_symbols`] on the accumulated bytes to
//! produce a sequence of [`Lz77Code`] entries that are then Huffman-coded.
//!
//! Adapted from `nopng::deflate::lz77_symbols`.

use crate::symbol::{MAX_MATCH, MIN_MATCH, WINDOW_SIZE};

const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const MAX_CHAIN_LEN: usize = 32;
const NIL: u32 = u32::MAX;

/// One LZ77 token: either a literal byte or a back-reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lz77Code {
    Literal(u8),
    Pointer { length: usize, distance: usize },
}

fn hash3(input: &[u8], pos: usize) -> usize {
    ((usize::from(input[pos]) << 10)
        ^ (usize::from(input[pos + 1]) << 5)
        ^ usize::from(input[pos + 2]))
        & HASH_MASK
}

/// Produce LZ77 tokens for the given input slice.
pub(crate) fn lz77_symbols(input: &[u8]) -> Vec<Lz77Code> {
    let mut symbols = Vec::new();
    if input.len() < MIN_MATCH {
        for &byte in input {
            symbols.push(Lz77Code::Literal(byte));
        }
        return symbols;
    }

    let mut head = vec![NIL; HASH_SIZE];
    let mut prev = vec![NIL; WINDOW_SIZE];
    let mut cursor = 0;

    while cursor < input.len() {
        if cursor + MIN_MATCH > input.len() {
            symbols.push(Lz77Code::Literal(input[cursor]));
            cursor += 1;
            continue;
        }

        let h = hash3(input, cursor);
        let max_length = (input.len() - cursor).min(MAX_MATCH);
        let search_start = cursor.saturating_sub(WINDOW_SIZE);

        let mut best_length = 0;
        let mut best_distance = 0;
        let mut chain_pos = head[h];
        let mut chain_count = 0;

        while chain_pos != NIL
            && (chain_pos as usize) >= search_start
            && (chain_pos as usize) < cursor
            && chain_count < MAX_CHAIN_LEN
        {
            let candidate = chain_pos as usize;
            if input[candidate] == input[cursor] {
                let mut length = 1;
                while length < max_length && input[candidate + length] == input[cursor + length] {
                    length += 1;
                }
                if length >= MIN_MATCH && length > best_length {
                    best_length = length;
                    best_distance = cursor - candidate;
                    if length == max_length {
                        break;
                    }
                }
            }
            chain_pos = prev[candidate & (WINDOW_SIZE - 1)];
            chain_count += 1;
        }

        prev[cursor & (WINDOW_SIZE - 1)] = head[h];
        head[h] = cursor as u32;

        if best_length >= MIN_MATCH {
            for i in 1..best_length {
                if cursor + i + MIN_MATCH <= input.len() {
                    let ih = hash3(input, cursor + i);
                    prev[(cursor + i) & (WINDOW_SIZE - 1)] = head[ih];
                    head[ih] = (cursor + i) as u32;
                }
            }
            symbols.push(Lz77Code::Pointer {
                length: best_length,
                distance: best_distance,
            });
            cursor += best_length;
        } else {
            symbols.push(Lz77Code::Literal(input[cursor]));
            cursor += 1;
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::{Lz77Code, lz77_symbols};

    #[test]
    fn short_input_all_literals() {
        let input = b"ab";
        let symbols = lz77_symbols(input);
        assert_eq!(
            symbols,
            vec![Lz77Code::Literal(b'a'), Lz77Code::Literal(b'b')]
        );
    }

    #[test]
    fn repeated_run_matches() {
        let input = b"aaaaa";
        let symbols = lz77_symbols(input);
        assert!(matches!(symbols.first(), Some(Lz77Code::Literal(b'a'))));
        assert!(
            symbols
                .iter()
                .any(|c| matches!(c, Lz77Code::Pointer { distance: 1, .. }))
        );
    }

    #[test]
    fn distant_match() {
        let input = b"abcdefghijk_____abcdefghijk";
        let symbols = lz77_symbols(input);
        assert!(
            symbols
                .iter()
                .any(|c| matches!(c, Lz77Code::Pointer { length: 11, .. }))
        );
    }
}
