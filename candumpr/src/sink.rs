use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::config::{Interval, RetentionLimit};
use crate::recv::Timestamp;
use crate::retention::RetentionPolicy;
use crate::template;
use crate::writer::{FileWriter, StdoutWriter, Writer, ZstdWriter};

/// Where a [Sink] writes.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// When to flush the writer: on a timer, on a byte count, or not at all.
    pub flush_every: Interval,
    /// When to sync the writer. A sync always flushes first.
    pub sync_every: Interval,
    /// Whether activation failures that waiting could heal are retried, or should be fatal.
    pub retry_activation_failures: bool,
    /// Compress file output with zstd. Ignored for [Output::Stdout].
    pub compress: bool,
    /// When to finalize the current file and start a new one.
    pub rotation: Interval,
    /// When to delete old files from the output directory. Only applies to [Output::Template].
    pub retention: RetentionLimit,
}

pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

impl SinkConfig {
    pub fn new(output: Output) -> SinkConfig {
        SinkConfig {
            output,
            header: None,
            flush_every: Interval::Every(DEFAULT_FLUSH_INTERVAL),
            sync_every: Interval::Every(DEFAULT_SYNC_INTERVAL),
            retry_activation_failures: false,
            compress: false,
            rotation: Interval::Off,
            retention: RetentionLimit::Off,
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
            } => dir.join(template::render(
                template::next_index_in(dir, interface),
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
        let file = FileWriter::new(std::fs::File::create(&path)?, path.clone());
        tracing::info!(path = %path.display(), "created log file");
        if self.compress {
            Ok(Box::new(ZstdWriter::new(file)?))
        } else {
            Ok(Box::new(std::io::BufWriter::new(file)))
        }
    }
}

/// A [Sink] manages [Writer] operations to write formatted CAN frames to whatever writer is configured
pub struct Sink {
    config: SinkConfig,
    pub(crate) state: SinkState,
    retention: Option<RetentionPolicy>,
    /// One flag for each interface that this Sink handles traffic for.
    address_claim_flags: Vec<Arc<AtomicBool>>,
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
        bytes_since_sync: usize,
        last_flush: Instant,
        last_sync: Instant,
        opened_at: Instant,
    },
    Closed,
}

impl Sink {
    /// Construct a Sink in the Pending state. Does no I/O.
    pub fn new(config: SinkConfig) -> Self {
        let retention = match &config.output {
            // In daemon mode, candumpr creates the <output dir>/<interface dir>/ directory, which
            // gives candumpr exclusive ownership of its contents. That makes it safe for the
            // RetentionPolicy to delete files in it. In CLI mode however, candumpr writes files to
            // the CWD, which we DO NOT want to delete files from.
            Output::Template { dir, .. } => {
                Some(RetentionPolicy::new(config.retention, dir.clone()))
            }
            Output::Stdout | Output::Path(_) => None,
        };
        Self {
            config,
            state: SinkState::Pending { last_attempt: None },
            retention,
            address_claim_flags: Vec::new(),
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
        let mut just_activated = false;

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
                bytes_since_sync: 0,
                last_flush: now,
                last_sync: now,
                opened_at: now,
            };
            just_activated = true;
        }

        let SinkState::Active {
            writer,
            bytes_since_flush,
            bytes_since_sync,
            last_flush,
            last_sync,
            ..
        } = &mut self.state
        else {
            unreachable!("state must be Active after the Pending branch above");
        };
        if just_activated {
            if let Some(policy) = &mut self.retention
                && let Some(path) = writer.path()
            {
                policy.activated(path)?;
            }
            for flag in &self.address_claim_flags {
                flag.store(true, Ordering::Relaxed);
            }
        }
        writer.write_all(bytes)?;
        wrote += bytes.len();
        *bytes_since_flush += wrote;
        *bytes_since_sync += wrote;
        if let Interval::Size(limit) = self.config.sync_every
            && *bytes_since_sync as u64 >= limit
        {
            writer.sync()?;
            *bytes_since_flush = 0;
            *bytes_since_sync = 0;
            let now = Instant::now();
            *last_flush = now;
            *last_sync = now;
        } else if let Interval::Size(limit) = self.config.flush_every
            && *bytes_since_flush as u64 >= limit
        {
            writer.flush()?;
            *bytes_since_flush = 0;
            *last_flush = Instant::now();
        }

        if let Some(policy) = &mut self.retention {
            policy.wrote(writer.bytes_written())?;
        }

        if self.should_rotate(Instant::now()) {
            self.rotate()?;
        }

        Ok(())
    }

    /// Whether the active file has hit its rotation limit.
    fn should_rotate(&self, now: Instant) -> bool {
        if !self.rotatable() {
            return false;
        }
        let SinkState::Active {
            writer, opened_at, ..
        } = &self.state
        else {
            return false;
        };
        match self.config.rotation {
            Interval::Off => false,
            Interval::Size(limit) => writer.bytes_written() >= limit,
            Interval::Every(limit) => now.duration_since(*opened_at) >= limit,
        }
    }

    /// Check the time-based flush and sync triggers
    ///
    /// Should be called periodically
    pub fn tick(&mut self) -> eyre::Result<()> {
        let now = Instant::now();

        if self.should_rotate(now) {
            // Rotation already flushes and syncs, and leaves the sink in SinkState::Pending
            return self.rotate();
        }

        let SinkState::Active {
            writer,
            bytes_since_flush,
            bytes_since_sync,
            last_flush,
            last_sync,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };

        if let Interval::Every(d) = self.config.sync_every
            && now.duration_since(*last_sync) >= d
        {
            writer.sync()?;
            *bytes_since_flush = 0;
            *bytes_since_sync = 0;
            *last_flush = now;
            *last_sync = now;
            return Ok(());
        }

        if let Interval::Every(d) = self.config.flush_every
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
            bytes_since_sync,
            last_flush,
            last_sync,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };
        writer.sync()?;
        *bytes_since_flush = 0;
        *bytes_since_sync = 0;
        let now = Instant::now();
        *last_flush = now;
        *last_sync = now;
        Ok(())
    }

    fn rotatable(&self) -> bool {
        // Only templated filenames that have the i<index> prefix can be rotated.
        matches!(self.config.output, Output::Template { .. })
    }

    /// Finalize the current file and return to Pending, so the next write opens a new one.
    pub fn rotate(&mut self) -> eyre::Result<()> {
        if !self.rotatable() {
            return Ok(());
        }
        let SinkState::Active { writer, .. } = &mut self.state else {
            return Ok(());
        };
        let result = writer.finish();
        // We can only .finish() once, so if it fails, we still have to transition into Pending.
        self.state = SinkState::Pending { last_attempt: None };
        Ok(result?)
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::recv::Timestamp;

    fn ts(sec: i64) -> Timestamp {
        Timestamp { sec, nsec: 0 }
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
        });
        config.flush_every = Interval::Off;
        config.sync_every = Interval::Off;
        let mut sink = Sink::new(config);

        assert!(sink.write(b"PAYLOAD", ts(42)).is_err());
    }

    /// A Template-output Sink logging into `dir`, with time-based flush/sync disabled.
    fn sink_in(dir: &TempDir, header: Option<Vec<u8>>) -> Sink {
        let mut config = SinkConfig::new(Output::Template {
            dir: dir.path().to_path_buf(),
            interface: "can0".to_string(),
            ext: "log".to_string(),
        });
        config.header = header;
        config.flush_every = Interval::Off;
        config.sync_every = Interval::Off;
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
    fn activation_picks_the_next_index_after_existing_logs() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        std::fs::write(dir.path().join("i0007_can0_<some timestamp>.log"), b"OLD").unwrap();

        sink.write(b"NEW", ts(1732117385)).unwrap();

        let names: Vec<_> = entries(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "i0007_can0_<some timestamp>.log",
                "i0008_can0_2024-11-20T15-43-05Z.log"
            ]
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
        config.flush_every = Interval::Off;
        config.sync_every = Interval::Off;
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
    fn rotation_finalizes_the_file_and_the_next_write_opens_the_next_index() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, Some(b"HDR".to_vec()));
        sink.write(b"FIRST", ts(1732117385)).unwrap();

        sink.rotate().unwrap();
        assert!(matches!(sink.state, SinkState::Pending { .. }));
        // rotate() finishes the writer, so the first file is complete without an explicit flush.
        assert_eq!(contents(&dir), b"HDRFIRST");

        sink.write(b"SECOND", ts(1732117385)).unwrap();
        sink.flush().unwrap();

        let paths = entries(&dir);
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "i0000_can0_2024-11-20T15-43-05Z.log",
                "i0001_can0_2024-11-20T15-43-05Z.log"
            ]
        );
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"HDRFIRST");
        // The header is rewritten at the top of the rotated file.
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"HDRSECOND");
    }

    #[test]
    fn activation_trips_claim_flags() {
        let dir = TempDir::new().unwrap();
        let mut sink = sink_in(&dir, None);
        let flag = Arc::new(AtomicBool::new(false));
        sink.address_claim_flags = vec![flag.clone()];

        sink.write(b"FIRST", ts(1732117385)).unwrap();
        assert!(
            flag.swap(false, Ordering::Relaxed),
            "the first write activates and must trip the flag"
        );

        sink.write(b"MORE", ts(1732117385)).unwrap();
        assert!(
            !flag.load(Ordering::Relaxed),
            "a write to an already-active sink must not trip the flag"
        );

        sink.rotate().unwrap();
        assert!(
            !flag.load(Ordering::Relaxed),
            "rotation alone must not trip the flag; the next activation does"
        );

        sink.write(b"SECOND", ts(1732117385)).unwrap();
        assert!(
            flag.load(Ordering::Relaxed),
            "the write after rotation re-activates and must trip the flag"
        );
    }

    fn rotating_sink_in(dir: &TempDir, rotation: Interval) -> Sink {
        let mut config = SinkConfig::new(Output::Template {
            dir: dir.path().to_path_buf(),
            interface: "can0".to_string(),
            ext: "log".to_string(),
        });
        // Every byte lands on disk immediately, so the tests can read files mid-stream.
        config.flush_every = Interval::Size(1);
        config.sync_every = Interval::Off;
        config.rotation = rotation;
        Sink::new(config)
    }

    #[test]
    fn size_rotation_starts_a_new_file_once_the_limit_is_crossed() {
        let dir = TempDir::new().unwrap();
        let mut sink = rotating_sink_in(&dir, Interval::Size(100));
        let half = [b'A'; 50];

        sink.write(&half, ts(1732117385)).unwrap();
        assert!(
            matches!(sink.state, SinkState::Active { .. }),
            "50 of 100 bytes is under the limit"
        );

        sink.write(&half, ts(1732117385)).unwrap();
        assert!(
            matches!(sink.state, SinkState::Pending { .. }),
            "writing the next 50 bytes should have rotated and finished the first file"
        );

        sink.write(b"NEXT", ts(1732117385)).unwrap();
        let paths = entries(&dir);
        assert_eq!(paths.len(), 2);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), [half, half].concat());
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"NEXT");
    }

    #[test]
    fn duration_rotation_fires_from_tick() {
        let dir = TempDir::new().unwrap();
        let mut sink = rotating_sink_in(&dir, Interval::Every(Duration::from_millis(1)));

        sink.write(b"PAYLOAD", ts(1732117385)).unwrap();
        assert!(matches!(sink.state, SinkState::Active { .. }));

        // TODO: Pass Instant::now() into tick()
        std::thread::sleep(Duration::from_millis(5));
        sink.tick().unwrap();

        assert!(matches!(sink.state, SinkState::Pending { .. }));
        // contents() asserts a single file is present in the directory
        assert_eq!(contents(&dir), b"PAYLOAD");
    }

    #[test]
    fn rotation_is_a_noop_for_path_output() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("can.log");
        let mut config = SinkConfig::new(Output::Path(path.clone()));
        config.flush_every = Interval::Off;
        config.sync_every = Interval::Off;
        let mut sink = Sink::new(config);

        sink.write(b"FIRST", ts(42)).unwrap();
        sink.rotate().unwrap();
        assert!(
            matches!(sink.state, SinkState::Active { .. }),
            "a non-rotatable sink stays Active"
        );
        sink.write(b"SECOND", ts(42)).unwrap();
        sink.flush().unwrap();

        assert_eq!(entries(&dir), vec![path.clone()]);
        assert_eq!(std::fs::read(&path).unwrap(), b"FIRSTSECOND");
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
