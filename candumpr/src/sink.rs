use std::time::{Duration, Instant};

use crate::recv::Timestamp;
use crate::writer::Writer;

/// A [Sink] manages [Writer] operations to write formatted CAN frames to whatever writer is configured
pub struct Sink {
    pub(crate) writer: Box<dyn Writer>,
    header: Option<Vec<u8>>,
    flush_threshold_bytes: usize,
    flush_interval: Option<Duration>,
    sync_interval: Option<Duration>,
    pub(crate) state: SinkState,
}

/// Lifecycle state of a [Sink].
pub(crate) enum SinkState {
    /// Writer exists, but no frame has been written yet
    Pending,
    /// Writer exists and is writing
    Active {
        bytes_since_flush: usize,
        last_flush: Instant,
        last_sync: Instant,
        /// Timestamp of the first frame seen by this sink, captured at activation.
        #[allow(dead_code)]
        timestamp: Timestamp,
    },
    Closed,
}

impl Sink {
    /// Construct a Sink in the Pending state with the given pre-built writer.
    pub fn new<W: Writer + 'static>(
        writer: W,
        header: Option<Vec<u8>>,
        flush_threshold_bytes: usize,
        flush_interval: Option<Duration>,
        sync_interval: Option<Duration>,
    ) -> Self {
        Self {
            writer: Box::new(writer),
            header,
            flush_threshold_bytes,
            flush_interval,
            sync_interval,
            state: SinkState::Pending,
        }
    }

    /// Write `bytes` to the writer, activating the sink on the first call.
    ///
    /// The bytes are expected to evenly divide CAN frames. That is, no partially formatted frames
    /// should be given [Self::write].
    pub fn write(&mut self, bytes: &[u8], timestamp: Timestamp) -> eyre::Result<()> {
        if matches!(self.state, SinkState::Closed) {
            eyre::bail!("write to closed sink");
        }

        let mut wrote = 0;

        if matches!(self.state, SinkState::Pending) {
            if let Some(header) = &self.header {
                self.writer.write(header)?;
                wrote += header.len();
            }
            let now = Instant::now();
            self.state = SinkState::Active {
                bytes_since_flush: 0,
                last_flush: now,
                last_sync: now,
                timestamp,
            };
        }

        self.writer.write(bytes)?;
        wrote += bytes.len();

        let SinkState::Active {
            bytes_since_flush,
            last_flush,
            ..
        } = &mut self.state
        else {
            unreachable!("state must be Active after the Pending branch above");
        };
        *bytes_since_flush += wrote;
        if *bytes_since_flush >= self.flush_threshold_bytes {
            self.writer.flush()?;
            *bytes_since_flush = 0;
            *last_flush = Instant::now();
        }

        Ok(())
    }

    /// Check the time-based flush and sync triggers
    ///
    /// Should be called periodically
    pub fn tick(&mut self) -> eyre::Result<()> {
        let SinkState::Active {
            bytes_since_flush,
            last_flush,
            last_sync,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };

        let now = Instant::now();

        if let Some(d) = self.sync_interval
            && now.duration_since(*last_sync) >= d
        {
            self.writer.sync()?;
            *bytes_since_flush = 0;
            *last_flush = now;
            *last_sync = now;
            return Ok(());
        }

        if let Some(d) = self.flush_interval
            && now.duration_since(*last_flush) >= d
        {
            self.writer.flush()?;
            *bytes_since_flush = 0;
            *last_flush = now;
        }

        Ok(())
    }

    /// Flush the writer if Active; no-op otherwise.
    pub fn flush(&mut self) -> eyre::Result<()> {
        let SinkState::Active {
            bytes_since_flush,
            last_flush,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };
        self.writer.flush()?;
        *bytes_since_flush = 0;
        *last_flush = Instant::now();
        Ok(())
    }

    /// Sync the writer if Active; no-op otherwise
    pub fn sync(&mut self) -> eyre::Result<()> {
        let SinkState::Active {
            bytes_since_flush,
            last_flush,
            last_sync,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };
        self.writer.sync()?;
        *bytes_since_flush = 0;
        let now = Instant::now();
        *last_flush = now;
        *last_sync = now;
        Ok(())
    }

    /// Finalize the writer and transition to Closed.
    pub fn close(&mut self) -> eyre::Result<()> {
        let result = match self.state {
            SinkState::Active { .. } => self.writer.finish(),
            SinkState::Pending | SinkState::Closed => Ok(()),
        };
        self.state = SinkState::Closed;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recv::Timestamp;
    use crate::test_util::TestBufWriter;

    fn ts(sec: i64) -> Timestamp {
        Timestamp { sec, nsec: 0 }
    }

    fn sink(header: Option<Vec<u8>>) -> Sink {
        Sink::new(TestBufWriter::new(), header, 64 * 1024, None, None)
    }

    fn bytes_in(sink: &mut Sink) -> Vec<u8> {
        sink.writer
            .as_any_mut()
            .downcast_mut::<TestBufWriter>()
            .unwrap()
            .bytes
            .clone()
    }

    #[test]
    fn header_written_on_activation() {
        let mut sink = sink(Some(b"HDR".to_vec()));
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        assert!(matches!(sink.state, SinkState::Active { .. }));
        assert_eq!(bytes_in(&mut sink), b"HDRPAYLOAD");
    }

    #[test]
    fn write_after_close_on_active_returns_err() {
        let mut sink = sink(None);
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        sink.close().unwrap();
        assert!(matches!(sink.state, SinkState::Closed));
        assert!(sink.write(b"MORE", ts(43)).is_err());
    }

    #[test]
    fn write_after_close_on_pending_returns_err() {
        let mut sink = sink(None);
        sink.close().unwrap();
        assert!(matches!(sink.state, SinkState::Closed));
        assert!(sink.write(b"PAYLOAD", ts(42)).is_err());
    }
}
