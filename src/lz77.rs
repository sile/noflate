//! LZ77 match finder used by the encoder.
//!
//! [`MatchFinder`] owns the hash-chain tables (`head` + `prev`, up to
//! 256 KiB combined) so they can be reused across multiple encode calls.
//! The tables are allocated lazily on the first call that actually needs
//! them, and `prev` grows only up to the input length (capped at
//! `WINDOW_SIZE`) — short one-shot encodes don't pay for the full window.
//! Callers provide the `Vec<Lz77Code>` scratch buffer so the token
//! allocation can also be reused across encode passes.
//!
//! Adapted from `nopng::deflate::lz77_symbols`; the matching strategy is
//! unchanged.

use alloc::vec;
use alloc::vec::Vec;

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

/// Longest common prefix length of `input[a..]` and `input[b..]` up to
/// `max` bytes. Reads 8 bytes at a time with `u64::from_le_bytes` and
/// locates the first mismatching byte via `trailing_zeros` on the XOR
/// -- stable, safe, no platform-specific intrinsics.
fn longest_common_prefix(input: &[u8], a: usize, b: usize, max: usize) -> usize {
    debug_assert!(a < b);
    let bounded = max.min(input.len() - b);
    let mut i = 0;
    while i + 8 <= bounded {
        let av = u64::from_le_bytes(input[a + i..a + i + 8].try_into().expect("8 bytes"));
        let bv = u64::from_le_bytes(input[b + i..b + i + 8].try_into().expect("8 bytes"));
        let diff = av ^ bv;
        if diff != 0 {
            return i + (diff.trailing_zeros() as usize / 8);
        }
        i += 8;
    }
    while i < bounded && input[a + i] == input[b + i] {
        i += 1;
    }
    i
}

/// Hash-chain match finder with reusable internal tables.
#[derive(Debug)]
pub(crate) struct MatchFinder {
    /// `HASH_SIZE` slots once allocated; empty before the first call
    /// that needs hashing (input shorter than `MIN_MATCH` skips the
    /// allocation entirely).
    head: Vec<u32>,
    /// Up to `WINDOW_SIZE` slots; grown on demand to cover the longest
    /// input seen so far so that short one-shot encodes don't pay for
    /// the full 32 KiB window.
    prev: Vec<u32>,
    /// `head` has been dirtied by a previous `fill_symbols` call and
    /// needs to be re-filled with `NIL` before the next run. False
    /// while `head` is empty or freshly allocated.
    head_dirty: bool,
}

impl MatchFinder {
    pub(crate) fn new() -> Self {
        Self {
            head: Vec::new(),
            prev: Vec::new(),
            head_dirty: false,
        }
    }

    /// Produce LZ77 tokens for `input` into `symbols`. The hash tables
    /// are allocated lazily on the first call with `input.len() >=
    /// MIN_MATCH` and reused thereafter. `head` is reset only when
    /// dirtied by a prior call; `prev` does not need to be reset
    /// because every entry is overwritten before being read (each
    /// position writes `prev[pos & mask]` before later code walks the
    /// chain through that same index).
    pub(crate) fn fill_symbols(&mut self, input: &[u8], symbols: &mut Vec<Lz77Code>) {
        symbols.clear();
        symbols.reserve(input.len().saturating_sub(symbols.capacity()));
        if input.len() < MIN_MATCH {
            for &byte in input {
                symbols.push(Lz77Code::Literal(byte));
            }
            return;
        }

        if self.head.is_empty() {
            self.head = vec![NIL; HASH_SIZE];
        } else if self.head_dirty {
            self.head.iter_mut().for_each(|slot| *slot = NIL);
        }
        self.head_dirty = true;

        // `prev` is indexed by `pos & (WINDOW_SIZE - 1)`. For inputs
        // shorter than `WINDOW_SIZE` the mask is a no-op, so we only
        // need `input.len()` slots; for longer inputs we need the full
        // window. Grow lazily to cover this call without ever shrinking.
        let prev_needed = input.len().min(WINDOW_SIZE);
        if self.prev.len() < prev_needed {
            self.prev.resize(prev_needed, NIL);
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
                // Pre-check `best_length` position to short-circuit
                // candidates that can't possibly beat the current best.
                // Without this, the u64 LCP call would run its full loop
                // even for obvious misses.
                if best_length == 0
                    || (input[candidate + best_length] == input[cursor + best_length]
                        && input[candidate] == input[cursor])
                {
                    let length = longest_common_prefix(input, candidate, cursor, max_length);
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
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{Lz77Code, MatchFinder};

    fn symbols(input: &[u8]) -> Vec<Lz77Code> {
        let mut matcher = MatchFinder::new();
        let mut symbols = Vec::new();
        matcher.fill_symbols(input, &mut symbols);
        symbols
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
        let mut syms = Vec::new();
        for _ in 0..3 {
            m.fill_symbols(b"banana banana", &mut syms);
            assert!(syms.iter().any(|c| matches!(c, Lz77Code::Pointer { .. })));
        }
    }
}
