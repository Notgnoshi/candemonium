use std::io::Write;
use std::time::{Duration, Instant};

use crate::recv::Timestamp;
use crate::writer::Writer;

/// Configuration for a [Sink].
pub struct SinkConfig {
    /// Format header written at activation, before the first frame.
    //
    // TODO: When the header includes dynamic data (like timestamp in ASC), we'll need to generate
    // the header on the first formatted frame the sink receives from the formatter.
    pub header: Option<Vec<u8>>,
    pub flush_threshold_bytes: usize,
    pub flush_interval: Option<Duration>,
    pub sync_interval: Option<Duration>,
}

impl SinkConfig {
    pub fn new() -> SinkConfig {
        SinkConfig {
            header: None,
            flush_threshold_bytes: 64 * 1024,
            flush_interval: Some(Duration::from_secs(5)),
            sync_interval: Some(Duration::from_secs(5 * 60)),
        }
    }
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A [Sink] manages [Writer] operations to write formatted CAN frames to whatever writer is configured
pub struct Sink {
    pub(crate) writer: Box<dyn Writer>,
    config: SinkConfig,
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
    pub fn new<W: Writer + 'static>(writer: W, config: SinkConfig) -> Self {
        Self {
            writer: Box::new(writer),
            config,
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
            if let Some(header) = &self.config.header {
                self.writer.write_all(header)?;
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

        self.writer.write_all(bytes)?;
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
        if *bytes_since_flush >= self.config.flush_threshold_bytes {
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

        if let Some(d) = self.config.sync_interval
            && now.duration_since(*last_sync) >= d
        {
            self.writer.sync()?;
            *bytes_since_flush = 0;
            *last_flush = now;
            *last_sync = now;
            return Ok(());
        }

        if let Some(d) = self.config.flush_interval
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
        Ok(result?)
    }
}

/// Render seconds since the epoch as UTC at second precision, with dashes for colons
fn iso_utc(sec: i64) -> String {
    let ts = jiff::Timestamp::from_second(sec).unwrap_or_else(|_| {
        let clamped = if sec < 0 {
            jiff::Timestamp::MIN
        } else {
            jiff::Timestamp::MAX
        };
        tracing::warn!("timestamp {sec}s since the epoch is out of range; clamping to {clamped}");
        clamped
    });
    ts.strftime("%Y-%m-%dT%H-%M-%SZ").to_string()
}

fn template_filename(index: u64, interface: &str, sec: i64, ext: &str) -> String {
    format!("i{index:04}_{interface}_{}.{ext}", iso_utc(sec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recv::Timestamp;
    use crate::test_util::TestBufWriter;

    fn ts(sec: i64) -> Timestamp {
        Timestamp { sec, nsec: 0 }
    }

    #[test]
    fn iso_utc_renders_and_clamps() {
        let vectors = [
            (0, "1970-01-01T00-00-00Z"),
            (1732117385, "2024-11-20T15-43-05Z"),
            (-86400, "1969-12-31T00-00-00Z"),
            (i64::MIN, "-9999-01-02T01-59-59Z"),
            (i64::MAX, "9999-12-30T22-00-00Z"),
        ];
        for (sec, expected) in vectors {
            assert_eq!(iso_utc(sec), expected, "sec={sec}");
        }
    }

    #[test]
    fn template_filename_renders_the_fixed_scheme() {
        assert_eq!(
            template_filename(0, "can0", 1732117385, "log"),
            "i0000_can0_2024-11-20T15-43-05Z.log"
        );
        // The index pads to width 4 and keeps counting past 9999 as plain decimal.
        assert_eq!(
            template_filename(10000, "vcan1", 0, "pcap"),
            "i10000_vcan1_1970-01-01T00-00-00Z.pcap"
        );
    }

    fn sink(header: Option<Vec<u8>>) -> Sink {
        let mut config = SinkConfig::new();
        config.header = header;
        config.flush_interval = None;
        config.sync_interval = None;
        Sink::new(TestBufWriter::new(), config)
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
