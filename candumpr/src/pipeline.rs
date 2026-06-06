use crate::format::Formatter;
use crate::frame::CanFrame;
use crate::recv::Timestamp;
use crate::sink::Sink;

/// Orchestrates formatting batches of [CanFrame]s and then writing them to each [Sink]
pub struct Pipeline<F: Formatter> {
    formatter: F,
    pub(crate) sinks: Vec<Sink>,
    bufs: Vec<Vec<u8>>,
    first_ts: Vec<Option<Timestamp>>,
}

impl<F: Formatter> Pipeline<F> {
    /// Construct a Pipeline over a non-empty set of sinks.
    ///
    /// There should either be one sink, or a sink for every CAN interface being logged.
    pub fn new(formatter: F, sinks: Vec<Sink>) -> Self {
        assert!(!sinks.is_empty(), "Pipeline requires at least one Sink");
        let n = sinks.len();
        let bufs = (0..n).map(|_| Vec::with_capacity(4096)).collect();
        let first_ts = vec![None; n];
        Self {
            formatter,
            sinks,
            bufs,
            first_ts,
        }
    }

    /// Write the given batch of [CanFrame]s to the [Sink]s
    ///
    /// With a single sink, every frame is routed to it regardless of interface index; this is both
    /// the common stdout case and what keeps a frame's [CanFrame::sock_id] from indexing past the
    /// only buffer. With multiple sinks, frames are dispatched by interface index.
    pub fn write_batch(&mut self, frames: &[CanFrame]) -> eyre::Result<()> {
        for buf in &mut self.bufs {
            buf.clear();
        }
        for slot in &mut self.first_ts {
            *slot = None;
        }

        let single = self.sinks.len() == 1;
        for frame in frames {
            let idx = if single { 0 } else { frame.sock_id };
            if self.first_ts[idx].is_none() {
                self.first_ts[idx] = Some(frame.timestamp);
            }
            self.formatter.format(frame, &mut self.bufs[idx]);
        }

        let mut first_err: Option<eyre::Report> = None;
        for (i, (sink, buf)) in self.sinks.iter_mut().zip(self.bufs.iter()).enumerate() {
            if buf.is_empty() {
                continue;
            }
            // first_ts[i] is Some whenever bufs[i] is non-empty: both are written together above.
            if let Err(e) = sink.write(buf, self.first_ts[i].unwrap()) {
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
        for sink in &mut self.sinks {
            if let Err(e) = op(sink) {
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

    use super::*;
    use crate::can::LinuxCanFrame;
    use crate::format::{CanutilsFileFormatter, TimestampMode};
    use crate::frame::Direction;
    use crate::sink::Sink;
    use crate::test_util::TestBufWriter;

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

    fn sink() -> Sink {
        Sink::new(TestBufWriter::new(), None, 64 * 1024, None, None)
    }

    fn bytes_in(sink: &mut Sink) -> Vec<u8> {
        sink.writer
            .as_any_mut()
            .downcast_mut::<TestBufWriter>()
            .unwrap()
            .bytes
            .clone()
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
        let mut pipeline = Pipeline::new(
            CanutilsFileFormatter::new(names.clone(), TimestampMode::Absolute),
            vec![sink()],
        );

        let frames = vec![frame(0, 0x100, &[0x01]), frame(3, 0x200, &[0x02])];
        pipeline.write_batch(&frames).unwrap();

        let expected = formatted(&names, &[&frames[0], &frames[1]]);
        assert_eq!(bytes_in(&mut pipeline.sinks[0]), expected);
    }

    #[test]
    fn per_interface_dispatches_by_sock_id() {
        let names = vec!["can0".to_string(), "can1".to_string(), "can2".to_string()];
        let sinks = vec![sink(), sink(), sink()];
        let mut pipeline = Pipeline::new(
            CanutilsFileFormatter::new(names.clone(), TimestampMode::Absolute),
            sinks,
        );

        let frames = vec![
            frame(0, 0x100, &[0x0A]),
            frame(2, 0x200, &[0x0B]),
            frame(0, 0x300, &[0x0C]),
            frame(1, 0x400, &[0x0D]),
        ];
        pipeline.write_batch(&frames).unwrap();

        assert_eq!(
            bytes_in(&mut pipeline.sinks[0]),
            formatted(&names, &[&frames[0], &frames[2]])
        );
        assert_eq!(
            bytes_in(&mut pipeline.sinks[1]),
            formatted(&names, &[&frames[3]])
        );
        assert_eq!(
            bytes_in(&mut pipeline.sinks[2]),
            formatted(&names, &[&frames[1]])
        );
    }
}
