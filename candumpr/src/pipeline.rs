use crate::format::Formatter;
use crate::frame::CanFrame;
use crate::recv::Timestamp;
use crate::sink::Sink;

/// One output stream: the [Formatter] for it, its [Sink], and the scratch state for the batch being
/// written.
struct Stream {
    formatter: Box<dyn Formatter>,
    sink: Sink,
    buf: Vec<u8>,
    first_ts: Option<Timestamp>,
}

/// Orchestrates formatting batches of [CanFrame]s and then writing them to each output stream
pub struct Pipeline {
    streams: Vec<Stream>,
}

impl Pipeline {
    /// Construct a Pipeline over a non-empty set of output streams.
    ///
    /// There should either be one stream, or a stream for every CAN interface being logged.
    pub fn new(streams: Vec<(Box<dyn Formatter>, Sink)>) -> Self {
        assert!(
            !streams.is_empty(),
            "Pipeline requires at least one output stream"
        );
        let streams = streams
            .into_iter()
            .map(|(formatter, sink)| Stream {
                formatter,
                sink,
                buf: Vec::with_capacity(4096),
                first_ts: None,
            })
            .collect();
        Self { streams }
    }

    /// Write the given batch of [CanFrame]s to the output streams
    ///
    /// With a single stream, every frame is routed to it regardless of interface index. With
    /// multiple streams, frames are dispatched by interface index.
    pub fn write_batch(&mut self, frames: &[CanFrame]) -> eyre::Result<()> {
        for stream in &mut self.streams {
            stream.buf.clear();
            stream.first_ts = None;
        }

        let single = self.streams.len() == 1;
        for frame in frames {
            let stream = &mut self.streams[if single { 0 } else { frame.sock_id }];
            if stream.first_ts.is_none() {
                stream.first_ts = Some(frame.timestamp);
            }
            stream.formatter.format(frame, &mut stream.buf);
        }

        let mut first_err: Option<eyre::Report> = None;
        for stream in &mut self.streams {
            if stream.buf.is_empty() {
                continue;
            }
            // first_ts is Some whenever buf is non-empty: both are written together above.
            if let Err(e) = stream.sink.write(&stream.buf, stream.first_ts.unwrap()) {
                match &first_err {
                    None => first_err = Some(e),
                    Some(_) => tracing::error!(error = ?e, "sink write failed"),
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Run each sink's periodic flush and sync checks.
    pub fn tick(&mut self) -> eyre::Result<()> {
        self.for_each_sink(Sink::tick)
    }

    /// Flush every sink.
    pub fn flush(&mut self) -> eyre::Result<()> {
        self.for_each_sink(Sink::flush)
    }

    /// Sync every sink.
    pub fn sync(&mut self) -> eyre::Result<()> {
        self.for_each_sink(Sink::sync)
    }

    /// Close every sink, finalizing each writer.
    pub fn close(&mut self) -> eyre::Result<()> {
        self.for_each_sink(Sink::close)
    }

    fn for_each_sink(
        &mut self,
        mut op: impl FnMut(&mut Sink) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let mut first_err: Option<eyre::Report> = None;
        for stream in &mut self.streams {
            if let Err(e) = op(&mut stream.sink) {
                match &first_err {
                    None => first_err = Some(e),
                    Some(_) => tracing::error!(error = ?e, "sink operation failed"),
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;
    use crate::can::LinuxCanFrame;
    use crate::format::{CanutilsFileFormatter, TimestampMode};
    use crate::frame::Direction;
    use crate::sink::{Output, Sink, SinkConfig};

    fn frame(sock_id: usize, id: u32, data: &[u8]) -> CanFrame {
        CanFrame {
            sock_id,
            timestamp: Timestamp {
                sec: 1000 + sock_id as i64,
                nsec: 0,
            },
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(id | libc::CAN_EFF_FLAG, data),
        }
    }

    /// A Path-output Sink writing to `sink<i>.log` in `dir`, with time-based flush/sync disabled.
    fn sink(dir: &TempDir, i: usize) -> Sink {
        let mut config = SinkConfig::new(Output::Path(dir.path().join(format!("sink{i}.log"))));
        config.flush_interval = None;
        config.sync_interval = None;
        Sink::new(config)
    }

    /// Contents of sink `i`'s file. The pipeline must be flushed first.
    fn bytes_in(dir: &TempDir, i: usize) -> Vec<u8> {
        std::fs::read(dir.path().join(format!("sink{i}.log"))).unwrap()
    }

    fn formatted(names: &[String], frames: &[&CanFrame]) -> Vec<u8> {
        let mut fmt = CanutilsFileFormatter::new(names.to_vec(), TimestampMode::Absolute);
        let mut buf = Vec::new();
        for frame in frames {
            fmt.format(frame, &mut buf);
        }
        buf
    }

    #[test]
    fn single_sink_dispatches_all_frames_to_index_zero() {
        let names = vec![
            "can0".to_string(),
            "can1".to_string(),
            "can2".to_string(),
            "can3".to_string(),
        ];
        let dir = TempDir::new().unwrap();
        let mut pipeline = Pipeline::new(vec![(
            Box::new(CanutilsFileFormatter::new(
                names.clone(),
                TimestampMode::Absolute,
            )),
            sink(&dir, 0),
        )]);

        let frames = vec![frame(0, 0x100, &[0x01]), frame(3, 0x200, &[0x02])];
        pipeline.write_batch(&frames).unwrap();
        pipeline.flush().unwrap();

        let expected = formatted(&names, &[&frames[0], &frames[1]]);
        assert_eq!(bytes_in(&dir, 0), expected);
    }

    #[test]
    fn per_interface_dispatches_by_sock_id() {
        let names = vec!["can0".to_string(), "can1".to_string(), "can2".to_string()];
        let dir = TempDir::new().unwrap();
        let streams = (0..3)
            .map(|i| {
                let formatter: Box<dyn Formatter> = Box::new(CanutilsFileFormatter::new(
                    names.clone(),
                    TimestampMode::Absolute,
                ));
                (formatter, sink(&dir, i))
            })
            .collect();
        let mut pipeline = Pipeline::new(streams);

        let frames = vec![
            frame(0, 0x100, &[0x0A]),
            frame(2, 0x200, &[0x0B]),
            frame(0, 0x300, &[0x0C]),
            frame(1, 0x400, &[0x0D]),
        ];
        pipeline.write_batch(&frames).unwrap();
        pipeline.flush().unwrap();

        assert_eq!(
            bytes_in(&dir, 0),
            formatted(&names, &[&frames[0], &frames[2]])
        );
        assert_eq!(bytes_in(&dir, 1), formatted(&names, &[&frames[3]]));
        assert_eq!(bytes_in(&dir, 2), formatted(&names, &[&frames[1]]));
    }
}
