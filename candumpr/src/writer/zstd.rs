use std::io::Write;

use zstd::stream::raw::CParameter;

use super::Writer;

// TODO: Tune these given real sample data
const LEVEL: i32 = 1;
const WINDOW_LOG: u32 = 15;

/// Compresses formatted frames with streaming zstd, one zstd frame per file.
pub struct ZstdWriter<W: Writer> {
    // Write to a Vec<u8> instead of the inner Writer so that we can always write chunks of
    // compressed data evenly divisible by CanFrames (no partial frames are written). We do this by
    // consuming from the Vec only upon flush(), which is only ever called at a Frame boundary.
    encoder: zstd::stream::write::Encoder<'static, Vec<u8>>,
    inner: W,
}

impl<W: Writer> ZstdWriter<W> {
    pub fn new(inner: W) -> std::io::Result<Self> {
        Self::with_params(inner, LEVEL, WINDOW_LOG)
    }

    pub fn with_params(inner: W, level: i32, window_log: u32) -> std::io::Result<Self> {
        let mut encoder = zstd::stream::raw::Encoder::new(level)?;
        encoder.set_parameter(CParameter::WindowLog(window_log))?;
        Ok(Self {
            encoder: zstd::stream::write::Encoder::with_encoder(Vec::new(), encoder),
            inner,
        })
    }
}

impl<W: Writer> std::io::Write for ZstdWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.encoder.write(buf)
    }

    /// Close the current block and flush
    ///
    /// The [ZstdWriter] does not write to disk outside of [ZstdWriter::flush] in order to guarantee
    /// that a CAN frame is never partially written across two compressed chunks.
    fn flush(&mut self) -> std::io::Result<()> {
        self.encoder.flush()?;
        let buf = self.encoder.get_mut();
        // We can't use Write::write_all, because in the case of a partial write it does not
        // indicate how much was written. So we mirror the implementation of BufWriter::flush_buf
        // which lets us retain unwritten bytes and retry. If we dropped bytes on write failure,
        // even if we started writing bytes again afterwards, everything after that point would be
        // corrupted.
        let mut written = 0;
        let result = loop {
            if written == buf.len() {
                break Ok(());
            }
            match self.inner.write(&buf[written..]) {
                Ok(0) => {
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write compressed data",
                    ));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        buf.drain(..written);
        result?;
        self.inner.flush()
    }
}

impl<W: Writer> Writer for ZstdWriter<W> {
    fn sync(&mut self) -> std::io::Result<()> {
        self.flush()?;
        self.inner.sync()
    }

    /// Finish writing.
    ///
    /// Flushes internal buffers, and writes the zstd frame epilogue. Do not write additional data
    /// after calling finish().
    fn finish(&mut self) -> std::io::Result<()> {
        self.encoder.do_finish()?;
        self.flush()?;
        self.inner.finish()
    }

    fn bytes_written(&self) -> u64 {
        self.inner.bytes_written()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recv::receiver::BATCH_CAPACITY;
    use crate::writer::FileWriter;

    /// Formatted candump frames for `range`, grouped the way a [Sink](crate::sink::Sink) writes
    /// them: one `Vec` per receive batch.
    fn batches(range: std::ops::Range<usize>) -> Vec<Vec<u8>> {
        range
            .map(|i| format!("(1732117385.{:06}) vcan0 123#{i:08X}\n", i % 1_000_000))
            .collect::<Vec<_>>()
            .chunks(BATCH_CAPACITY)
            .map(|batch| batch.concat().into_bytes())
            .collect()
    }

    /// The inner [Writer] under a [ZstdWriter] under test.
    ///
    /// Keeps what it accepts, and counts `write` calls, which is how many times compressed bytes
    /// left the [ZstdWriter].
    #[derive(Default)]
    struct WriteCountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        /// When set, the first `write` accepts only this many bytes and the second fails with EIO
        flaky: Option<usize>,
    }

    impl std::io::Write for WriteCountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            let accepted = match (self.flaky, self.writes) {
                (Some(n), 1) => n.min(buf.len()),
                (Some(_), 2) => return Err(std::io::Error::from_raw_os_error(libc::EIO)),
                _ => buf.len(),
            };
            self.bytes.extend_from_slice(&buf[..accepted]);
            Ok(accepted)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Writer for WriteCountingWriter {
        fn sync(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn finish(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn bytes_written(&self) -> u64 {
            self.bytes.len() as u64
        }
    }

    // happy path
    #[test]
    fn round_trip_decodes_to_input() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let batches = batches(0..1000);

        let mut writer = ZstdWriter::new(FileWriter::new(file.reopen().unwrap())).unwrap();
        for batch in &batches {
            writer.write_all(batch).unwrap();
        }
        writer.finish().unwrap();

        let raw = std::fs::read(file.path()).unwrap();
        assert_eq!(zstd::decode_all(&raw[..]).unwrap(), batches.concat());
    }

    // recovery after write errors shouldn't result in something that can't decompress
    #[test]
    fn partial_write_retains_the_remainder() {
        let mut writer = ZstdWriter::new(WriteCountingWriter {
            flaky: Some(100),
            ..WriteCountingWriter::default()
        })
        .unwrap();
        let input = batches(0..1000).concat();
        writer.write_all(&input).unwrap();

        let err = writer.flush().unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EIO));
        assert_eq!(writer.inner.bytes.len(), 100, "kept only what was accepted");

        // Retrying recovers everything: the bytes the inner writer refused are still buffered.
        // Dropping them instead would leave a hole that costs every frame after it.
        writer.finish().unwrap();
        assert_eq!(zstd::decode_all(&writer.inner.bytes[..]).unwrap(), input);
    }
}
