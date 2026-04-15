//! Internal ring-style buffer shared by the sans-io encoder and decoder.
//!
//! Based on `rtmp-rs`'s `Buf`: bytes are appended via [`Buf::feed`] or
//! [`Buf::push`] / [`Buf::extend_from_slice`], exposed for borrowed reads via
//! [`Buf::get`], and removed from the front by [`Buf::advance`]. To keep
//! memory bounded when readers lag behind writers, the buffer compacts once
//! its underlying `Vec` exceeds 1 MiB.

use alloc::vec::Vec;

#[derive(Debug, Default)]
pub(crate) struct Buf {
    bytes: Vec<u8>,
    offset: usize,
}

impl Buf {
    const COMPACT_THRESHOLD: usize = 1024 * 1024;

    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    pub(crate) fn feed(&mut self, buf: &[u8]) {
        self.bytes.extend_from_slice(buf);
        self.maybe_compact();
    }

    pub(crate) fn advance(&mut self, n: usize) {
        assert!(
            self.offset + n <= self.bytes.len(),
            "advance past end of buffer: offset={}, n={}, len={}",
            self.offset,
            n,
            self.bytes.len(),
        );
        self.offset += n;
        if self.offset == self.bytes.len() {
            self.offset = 0;
            self.bytes.clear();
        }
    }

    fn maybe_compact(&mut self) {
        if self.bytes.len() > Self::COMPACT_THRESHOLD && self.offset > 0 {
            let remaining = self.bytes.len() - self.offset;
            self.bytes.copy_within(self.offset.., 0);
            self.bytes.truncate(remaining);
            self.offset = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::Buf;

    #[test]
    fn feed_and_get_roundtrip() {
        let mut buf = Buf::new();
        buf.feed(b"hello");
        assert_eq!(buf.get(), b"hello");
    }

    #[test]
    fn advance_drops_prefix() {
        let mut buf = Buf::new();
        buf.feed(b"abcdef");
        buf.advance(2);
        assert_eq!(buf.get(), b"cdef");
        buf.advance(4);
        assert!(buf.get().is_empty());
    }

    #[test]
    fn advance_to_empty_clears_bytes() {
        let mut buf = Buf::new();
        buf.feed(b"abc");
        buf.advance(3);
        assert_eq!(buf.get(), b"");
        buf.feed(b"xyz");
        assert_eq!(buf.get(), b"xyz");
    }

    #[test]
    #[should_panic]
    fn advance_past_end_panics() {
        let mut buf = Buf::new();
        buf.feed(b"ab");
        buf.advance(3);
    }

    #[test]
    fn compacts_when_past_threshold() {
        let mut buf = Buf::new();
        let chunk = vec![0u8; 1024];
        for _ in 0..1100 {
            buf.feed(&chunk);
        }
        buf.advance(1024 * 500);
        buf.feed(&chunk);
        assert!(buf.get().len() >= 1024);
    }
}
