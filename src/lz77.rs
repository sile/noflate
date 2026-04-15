//! LZ77 match finder used by the encoder.
//!
//! [`MatchFinder`] owns the hash-chain tables (`head` + `prev`, 256 KiB
//! combined) so they can be reused across multiple encode calls. For a
//! one-shot encode pass, [`MatchFinder::symbols`] emits the full
//! `Vec<Lz77Code>` for the input.
//!
//! Adapted from `nopng::deflate::lz77_symbols`; the matching strategy is
//! unchanged.

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

/// Hash-chain match finder with reusable internal tables.
#[derive(Debug)]
pub(crate) struct MatchFinder {
    head: Vec<u32>,
    prev: Vec<u32>,
    /// Head table has been dirtied by a previous `symbols` call and
    /// needs to be re-filled with `NIL` before the next run. False for a
    /// freshly-constructed matcher (the Vec was allocated with NIL).
    head_dirty: bool,
}

impl MatchFinder {
    pub(crate) fn new() -> Self {
        Self {
            head: vec![NIL; HASH_SIZE],
            prev: vec![NIL; WINDOW_SIZE],
            head_dirty: false,
        }
    }

    /// Produce LZ77 tokens for `input`. Previous contents of the hash
    /// tables are reset lazily; `prev` does not need to be reset because
    /// every entry is overwritten before being read (each position
    /// writes `prev[pos & mask]` before later code walks the chain
    /// through that same index).
    pub(crate) fn symbols(&mut self, input: &[u8]) -> Vec<Lz77Code> {
        if self.head_dirty {
            self.head.iter_mut().for_each(|slot| *slot = NIL);
        }
        self.head_dirty = true;
        let mut symbols = Vec::new();
        if input.len() < MIN_MATCH {
            for &byte in input {
                symbols.push(Lz77Code::Literal(byte));
            }
            return symbols;
        }

        let head = &mut self.head[..];
        let prev = &mut self.prev[..];
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
                    while length < max_length
                        && input[candidate + length] == input[cursor + length]
                    {
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
}

#[cfg(test)]
mod tests {
    use super::{Lz77Code, MatchFinder};

    fn symbols(input: &[u8]) -> Vec<Lz77Code> {
        MatchFinder::new().symbols(input)
    }

    #[test]
    fn short_input_all_literals() {
        let input = b"ab";
        assert_eq!(
            symbols(input),
            vec![Lz77Code::Literal(b'a'), Lz77Code::Literal(b'b')]
        );
    }

    #[test]
    fn repeated_run_matches() {
        let input = b"aaaaa";
        let syms = symbols(input);
        assert!(matches!(syms.first(), Some(Lz77Code::Literal(b'a'))));
        assert!(
            syms.iter()
                .any(|c| matches!(c, Lz77Code::Pointer { distance: 1, .. }))
        );
    }

    #[test]
    fn distant_match() {
        let input = b"abcdefghijk_____abcdefghijk";
        let syms = symbols(input);
        assert!(
            syms.iter()
                .any(|c| matches!(c, Lz77Code::Pointer { length: 11, .. }))
        );
    }

    #[test]
    fn matcher_reuses_tables_across_calls() {
        let mut m = MatchFinder::new();
        for _ in 0..3 {
            let syms = m.symbols(b"banana banana");
            assert!(syms.iter().any(|c| matches!(c, Lz77Code::Pointer { .. })));
        }
    }
}
