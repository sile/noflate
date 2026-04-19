//! Bridge noflate's sans-io `Encoder` / `Decoder` to `std::io::Write` and
//! `std::io::Read`. Run with `cargo run --example io_bridge`.
//!
//! noflate itself is `no_std` and performs no I/O — callers feed bytes in
//! and borrow bytes out. When you already have a `Write` sink or a `Read`
//! source, the adapters below show the minimal wiring to plug noflate in.
//! The same pattern works for `gzip::Encoder` / `Decoder` and
//! `zlib::Encoder` / `Decoder` — swap the module, the adapter code is
//! identical.

use std::io::{self, Read, Write};

/// Wraps a `Write` sink and compresses everything written to it with
/// DEFLATE. Call [`DeflateWriter::finish`] to emit the final block; the
/// stream is incomplete until then.
struct DeflateWriter<W: Write> {
    inner: W,
    encoder: noflate::deflate::Encoder,
}

impl<W: Write> DeflateWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            encoder: noflate::deflate::Encoder::new(),
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        let produced = self.encoder.output();
        if !produced.is_empty() {
            self.inner.write_all(produced)?;
            let n = produced.len();
            self.encoder.advance(n);
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        self.encoder.finish().map_err(io::Error::other)?;
        self.drain()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for DeflateWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.feed(buf).map_err(io::Error::other)?;
        self.drain()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Wraps a `Read` source and decompresses a DEFLATE stream on the fly.
struct DeflateReader<R: Read> {
    inner: R,
    decoder: noflate::deflate::Decoder,
    buf: [u8; 4096],
    eof: bool,
}

impl<R: Read> DeflateReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            decoder: noflate::deflate::Decoder::new(),
            buf: [0; 4096],
            eof: false,
        }
    }
}

impl<R: Read> Read for DeflateReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            let available = self.decoder.output();
            if !available.is_empty() {
                let n = available.len().min(out.len());
                out[..n].copy_from_slice(&available[..n]);
                self.decoder.advance(n);
                return Ok(n);
            }
            if self.decoder.is_finished() {
                return Ok(0);
            }
            if self.eof {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "deflate stream ended before the final block",
                ));
            }
            let n = self.inner.read(&mut self.buf)?;
            if n == 0 {
                self.eof = true;
                continue;
            }
            self.decoder.feed(&self.buf[..n]).map_err(io::Error::other)?;
        }
    }
}

fn main() -> io::Result<()> {
    let original: &[u8] = b"Hello, std::io! \
        The quick brown fox jumps over the lazy dog. \
        The quick brown fox jumps over the lazy dog.";

    // Compress through std::io::Write.
    let mut writer = DeflateWriter::new(Vec::new());
    writer.write_all(&original[..20])?;
    writer.write_all(&original[20..])?;
    let compressed = writer.finish()?;

    // Decompress through std::io::Read.
    let mut reader = DeflateReader::new(compressed.as_slice());
    let mut decompressed = Vec::new();
    reader.read_to_end(&mut decompressed)?;

    assert_eq!(decompressed, original);
    println!(
        "roundtrip ok: {} bytes -> {} bytes -> {} bytes",
        original.len(),
        compressed.len(),
        decompressed.len(),
    );
    Ok(())
}
