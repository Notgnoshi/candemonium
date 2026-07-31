use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::recv::Timestamp;
use crate::writer::{FileWriter, StdoutWriter, Writer, ZstdWriter};

/// Where a [Sink] writes.
pub enum Output {
    Stdout,
    /// An exact user-given path, truncated if it already exists.
    Path(PathBuf),
    /// Parameters to fill in the default path template
    Template {
        dir: PathBuf,
        interface: String,
        /// File extension to use
        ext: String,
        /// Index of the next file to create
        next_index: u64,
    },
}

/// Configuration for a [Sink].
pub struct SinkConfig {
    pub output: Output,
    /// Format header written at activation, before the first frame.
    //
    // TODO: When the header includes dynamic data (like timestamp in ASC), we'll need to generate
    // the header on the first formatted frame the sink receives from the formatter.
    pub header: Option<Vec<u8>>,
    pub flush_threshold_bytes: usize,
    pub flush_interval: Option<Duration>,
    pub sync_interval: Option<Duration>,
    /// Whether activation failures that waiting could heal are retried, or should be fatal.
    pub retry_activation_failures: bool,
    /// Compress file output with zstd. Ignored for [Output::Stdout].
    pub compress: bool,
}

impl SinkConfig {
    pub fn new(output: Output) -> SinkConfig {
        SinkConfig {
            output,
            header: None,
            flush_threshold_bytes: 64 * 1024,
            flush_interval: Some(Duration::from_secs(5)),
            sync_interval: Some(Duration::from_secs(5 * 60)),
            retry_activation_failures: false,
            compress: false,
        }
    }

    /// Construct the writer stack for this config's output.
    fn open_writer(&self, timestamp: Timestamp) -> std::io::Result<Box<dyn Writer>> {
        let path = match &self.output {
            // Unreachable with compression: main rejects --compress against stdout.
            Output::Stdout => return Ok(Box::new(StdoutWriter::new())),
            Output::Path(path) => path.clone(),
            Output::Template {
                dir,
                interface,
                ext,
                next_index,
            } => dir.join(template_filename(
                *next_index,
                interface,
                timestamp.sec,
                ext,
            )),
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = FileWriter::new(std::fs::File::create(&path)?);
        tracing::info!(path = %path.display(), "created log file");
        if self.compress {
            Ok(Box::new(ZstdWriter::new(file)?))
        } else {
            Ok(Box::new(std::io::BufWriter::with_capacity(
                self.flush_threshold_bytes,
                file,
            )))
        }
    }
}

/// A [Sink] manages [Writer] operations to write formatted CAN frames to whatever writer is configured
pub struct Sink {
    config: SinkConfig,
    pub(crate) state: SinkState,
}

/// Minimum time between activation attempts after a failed activation.
const ACTIVATION_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Lifecycle state of a [Sink].
pub(crate) enum SinkState {
    /// No writer exists; it is constructed from the output config on the first write.
    Pending {
        /// If we've tried to activate this [Sink] already, when was the last failed activation?
        last_attempt: Option<Instant>,
    },
    /// Writer exists and is writing
    Active {
        writer: Box<dyn Writer>,
        bytes_since_flush: usize,
        last_flush: Instant,
        last_sync: Instant,
    },
    Closed,
}

impl Sink {
    /// Construct a Sink in the Pending state. Does no I/O.
    pub fn new(config: SinkConfig) -> Self {
        Self {
            config,
            state: SinkState::Pending { last_attempt: None },
        }
    }

    /// Write `bytes` to the writer, activating the sink on the first call.
    ///
    /// The bytes are expected to evenly divide CAN frames. That is, no partially formatted frames
    /// should be given [Self::write].
    ///
    /// After activation, write errors are forwarded as `Err`s. But activation errors in particular
    /// are sometimes suppressed, depending on the value of [SinkConfig::retry_activation_failures].
    /// * When set, a recoverable error is suppressed, and the [Sink] attempts to reactivate after a
    ///   delay. While retrying, all CAN frames are irrecoverably dropped.
    /// * When unset, even recoverable errors are returned as `Err`s
    ///
    /// The intent is to allow the `Pipeline` to define an error handling policy based on whether
    /// it's running in interactive CLI mode (all errors are fatal), or background logging user-given
    /// mode (some errors can be recovered from).
    pub fn write(&mut self, bytes: &[u8], timestamp: Timestamp) -> eyre::Result<()> {
        if matches!(self.state, SinkState::Closed) {
            eyre::bail!("write to closed sink");
        }

        let mut wrote = 0;

        if let SinkState::Pending { last_attempt } = &self.state {
            if let Some(prev) = last_attempt
                && prev.elapsed() < ACTIVATION_RETRY_INTERVAL
            {
                // Between retries, drop the batch without touching the filesystem :(
                return Ok(());
            }
            self.state = SinkState::Pending {
                last_attempt: Some(Instant::now()),
            };
            let mut writer = match self.config.open_writer(timestamp) {
                Ok(writer) => writer,
                Err(e) => {
                    if classify(e.kind(), self.config.retry_activation_failures)
                        == ActivationFailure::Fatal
                    {
                        return Err(eyre::Report::new(e).wrap_err("sink activation failed"));
                    }
                    // I'm not especially comfortable with the failure mode being dropping the
                    // frames, but this class of error means we failed to open or write to the log
                    // file, which I want to be rare enough that it's not worth durably handling
                    // those failure cases.
                    tracing::warn!(
                        error = %e,
                        "sink activation failed; dropping frames until the next retry"
                    );
                    return Ok(());
                }
            };
            if let Some(header) = &self.config.header {
                writer.write_all(header)?;
                wrote += header.len();
            }
            let now = Instant::now();
            self.state = SinkState::Active {
                writer,
                bytes_since_flush: 0,
                last_flush: now,
                last_sync: now,
            };
        }

        let SinkState::Active {
            writer,
            bytes_since_flush,
            last_flush,
            ..
        } = &mut self.state
        else {
            unreachable!("state must be Active after the Pending branch above");
        };
        writer.write_all(bytes)?;
        wrote += bytes.len();
        *bytes_since_flush += wrote;
        if *bytes_since_flush >= self.config.flush_threshold_bytes {
            writer.flush()?;
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
            writer,
            bytes_since_flush,
            last_flush,
            last_sync,
        } = &mut self.state
        else {
            return Ok(());
        };

        let now = Instant::now();

        if let Some(d) = self.config.sync_interval
            && now.duration_since(*last_sync) >= d
        {
            writer.sync()?;
            *bytes_since_flush = 0;
            *last_flush = now;
            *last_sync = now;
            return Ok(());
        }

        if let Some(d) = self.config.flush_interval
            && now.duration_since(*last_flush) >= d
        {
            writer.flush()?;
            *bytes_since_flush = 0;
            *last_flush = now;
        }

        Ok(())
    }

    /// Flush the writer if Active; no-op otherwise.
    pub fn flush(&mut self) -> eyre::Result<()> {
        let SinkState::Active {
            writer,
            bytes_since_flush,
            last_flush,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };
        writer.flush()?;
        *bytes_since_flush = 0;
        *last_flush = Instant::now();
        Ok(())
    }

    /// Sync the writer if Active; no-op otherwise
    pub fn sync(&mut self) -> eyre::Result<()> {
        let SinkState::Active {
            writer,
            bytes_since_flush,
            last_flush,
            last_sync,
        } = &mut self.state
        else {
            return Ok(());
        };
        writer.sync()?;
        *bytes_since_flush = 0;
        let now = Instant::now();
        *last_flush = now;
        *last_sync = now;
        Ok(())
    }

    /// Finalize the writer and transition to Closed.
    pub fn close(&mut self) -> eyre::Result<()> {
        let result = match &mut self.state {
            SinkState::Active { writer, .. } => writer.finish(),
            SinkState::Pending { .. } | SinkState::Closed => Ok(()),
        };
        self.state = SinkState::Closed;
        Ok(result?)
    }
}

/// How a [Sink] responds to a failed activation.
#[derive(Debug, PartialEq, Eq)]
enum ActivationFailure {
    Retry,
    Fatal,
}

/// Classify an activation failure.
fn classify(kind: std::io::ErrorKind, retry_activation_failures: bool) -> ActivationFailure {
    if !retry_activation_failures {
        return ActivationFailure::Fatal;
    }
    match kind {
        // These errors require human intervention to fix. Most others *could* be resolved
        // automatically, so we continue to retry for those cases.
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotADirectory => {
            ActivationFailure::Fatal
        }
        _ => ActivationFailure::Retry,
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
    use tempfile::TempDir;

    use super::*;
    use crate::recv::Timestamp;

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
    fn fatal_activation_failure_returns_err() {
        // A regular file as a directory component: NotADirectory, a config error only a human
        // can fix. Reliable under any uid, unlike permission-based setups in the test namespace.
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"file, not a directory").unwrap();

        let mut config = SinkConfig::new(Output::Template {
            dir: blocker.join("logs"),
            interface: "can0".to_string(),
            ext: "log".to_string(),
            next_index: 0,
        });
        config.flush_interval = None;
        config.sync_interval = None;
        let mut sink = Sink::new(config);

        assert!(sink.write(b"PAYLOAD", ts(42)).is_err());
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

    /// A Template-output Sink logging into `dir`, with time-based flush/sync disabled.
    fn sink_in(dir: &TempDir, header: Option<Vec<u8>>) -> Sink {
        let mut config = SinkConfig::new(Output::Template {
            dir: dir.path().to_path_buf(),
            interface: "can0".to_string(),
            ext: "log".to_string(),
            next_index: 0,
        });
        config.header = header;
        config.flush_interval = None;
        config.sync_interval = None;
        Sink::new(config)
    }

    fn entries(dir: &TempDir) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        paths
    }

    /// Contents of the single file in `dir`.
    fn contents(dir: &TempDir) -> Vec<u8> {
        let paths = entries(dir);
        assert_eq!(paths.len(), 1, "expected exactly one file: {paths:?}");
        std::fs::read(&paths[0]).unwrap()
    }

    #[test]
    fn creation_deferred_until_first_write_then_named_from_first_frame() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        assert!(entries(&dir).is_empty(), "Sink::new must do no I/O");

        sink.write(b"PAYLOAD", ts(1732117385)).unwrap();
        let paths = entries(&dir);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0].file_name().unwrap(),
            "i0000_can0_2024-11-20T15-43-05Z.log"
        );
    }

    #[test]
    fn header_written_on_activation() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, Some(b"HDR".to_vec()));
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        assert!(matches!(sink.state, SinkState::Active { .. }));
        sink.flush().unwrap();
        assert_eq!(contents(&dir), b"HDRPAYLOAD");
    }

    #[test]
    fn path_output_uses_name_verbatim_and_truncates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("can.log");
        std::fs::write(&path, b"LEFTOVER FROM AN EARLIER RUN").unwrap();

        let mut config = SinkConfig::new(Output::Path(path.clone()));
        config.flush_interval = None;
        config.sync_interval = None;
        let mut sink = Sink::new(config);
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        sink.flush().unwrap();

        assert_eq!(entries(&dir), vec![path.clone()]);
        assert_eq!(std::fs::read(&path).unwrap(), b"PAYLOAD");
    }

    #[test]
    fn writes_buffer_until_flush() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        assert_eq!(
            contents(&dir),
            b"",
            "below the threshold, bytes stay buffered"
        );
        sink.flush().unwrap();
        assert_eq!(contents(&dir), b"PAYLOAD");
    }

    #[test]
    fn tick_flush_sync_are_noops_while_pending() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        sink.tick().unwrap();
        sink.flush().unwrap();
        sink.sync().unwrap();
        assert!(entries(&dir).is_empty());
    }

    #[test]
    fn write_after_close_on_active_returns_err() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        sink.write(b"PAYLOAD", ts(42)).unwrap();
        sink.close().unwrap();
        assert!(matches!(sink.state, SinkState::Closed));
        // finish() flushed and synced: contents are complete without an explicit flush.
        assert_eq!(contents(&dir), b"PAYLOAD");
        assert!(sink.write(b"MORE", ts(43)).is_err());
    }

    #[test]
    fn write_after_close_on_pending_returns_err() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        sink.close().unwrap();
        assert!(matches!(sink.state, SinkState::Closed));
        assert!(sink.write(b"PAYLOAD", ts(42)).is_err());
        assert!(
            entries(&dir).is_empty(),
            "closing a Pending sink creates nothing"
        );
    }
}
