use std::io::Write;
use std::time::Duration;

use crate::frame::CanFrame;
use crate::recv::Timestamp;

/// How candumpr renders the timestamp prefix on candump-format output.
///
/// Only the candump formats consult this. ASC and PCAP carry their own intrinsic timestamps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum TimestampMode {
    /// The frame's absolute receive time
    #[default]
    Absolute,
    /// Time since the previous frame
    Delta,
    /// Time since the first frame
    Zero,
}

/// A resolved timestamp ready to render
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisplayTimestamp {
    /// The frame's absolute receive time.
    Absolute(Timestamp),
    /// The elapsed duration since the reference frame.
    Relative(Duration),
}

/// Tracks the reference timestamp needed to render relative timestamps ([TimestampMode::Delta] and
/// [TimestampMode::Zero])
struct TimestampClock {
    mode: TimestampMode,
    /// For Zero: the first frame's time. For Delta: the previous frame's time.
    reference: Option<Timestamp>,
}

impl TimestampClock {
    fn new(mode: TimestampMode) -> Self {
        Self {
            mode,
            reference: None,
        }
    }

    /// Resolve the timestamp to render for `ts`, advancing internal state as needed.
    ///
    /// The first frame in a relative mode resolves to a zero duration, matching candump.
    fn resolve(&mut self, ts: Timestamp) -> DisplayTimestamp {
        match self.mode {
            TimestampMode::Absolute => DisplayTimestamp::Absolute(ts),
            TimestampMode::Zero => {
                let first = *self.reference.get_or_insert(ts);
                DisplayTimestamp::Relative(elapsed(first, ts))
            }
            TimestampMode::Delta => {
                let prev = self.reference.replace(ts).unwrap_or(ts);
                DisplayTimestamp::Relative(elapsed(prev, ts))
            }
        }
    }
}

/// Non-negative elapsed duration from `start` to `end`.
///
/// Clamps to zero if `end` precedes `start` (e.g. a backward system-clock jump).
fn elapsed(start: Timestamp, end: Timestamp) -> Duration {
    let start_ns = start.sec as i128 * 1_000_000_000 + start.nsec as i128;
    let end_ns = end.sec as i128 * 1_000_000_000 + end.nsec as i128;
    let diff_ns = (end_ns - start_ns).max(0) as u128;
    Duration::new(
        (diff_ns / 1_000_000_000) as u64,
        (diff_ns % 1_000_000_000) as u32,
    )
}

/// Write the candump-style `(seconds.microseconds) ` timestamp prefix, including the trailing space.
///
/// Absolute times print unpadded seconds; relative times zero-pad seconds to three digits, matching
/// candump's `(%llu.%06llu)` and `(%03llu.%06llu)` respectively.
fn write_timestamp(buf: &mut Vec<u8>, ts: DisplayTimestamp) {
    match ts {
        DisplayTimestamp::Absolute(t) => {
            write!(buf, "({}.{:06}) ", t.sec, t.nsec / 1000).unwrap();
        }
        DisplayTimestamp::Relative(d) => {
            write!(buf, "({:03}.{:06}) ", d.as_secs(), d.subsec_micros()).unwrap();
        }
    }
}

/// Formats received CAN frames into bytes for writing.
pub trait Formatter {
    /// Append the formatted representation of `frame` to `buf`.
    fn format(&self, frame: &CanFrame, buf: &mut Vec<u8>);

    /// Optional header bytes written once at the start of each output stream, before any frames.
    ///
    /// Used by formats with a file-level header (e.g. PCAP).
    fn header(&self) -> Option<&[u8]> {
        None
    }
}

/// Formats frames in the can-utils candump file format.
///
/// Output format: `(SECONDS.MICROSECONDS) IFACE CANID#DATA\n`
///
/// Example: `(1616161616.123456) can0 18FECA00#AABB0011`
pub struct CanutilsFormatter {
    iface_names: Vec<String>,
}

impl CanutilsFormatter {
    pub fn new(iface_names: Vec<String>) -> Self {
        Self { iface_names }
    }
}

impl Formatter for CanutilsFormatter {
    fn format(&self, frame: &CanFrame, buf: &mut Vec<u8>) {
        let ts = &frame.timestamp;
        let iface = &self.iface_names[frame.sock_id];
        let can_id = frame.raw.can_id;

        let (id, width) = if can_id & libc::CAN_ERR_FLAG != 0 {
            (can_id & (libc::CAN_ERR_MASK | libc::CAN_ERR_FLAG), 8)
        } else if can_id & libc::CAN_EFF_FLAG != 0 {
            (can_id & libc::CAN_EFF_MASK, 8)
        } else {
            (can_id & libc::CAN_SFF_MASK, 3)
        };

        write!(
            buf,
            "({}.{:06}) {} {:0width$X}#",
            ts.sec,
            ts.nsec / 1000,
            iface,
            id,
            width = width,
        )
        .unwrap();
        for i in 0..frame.raw.len as usize {
            write!(buf, "{:02X}", frame.raw.data[i]).unwrap();
        }
        buf.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::can::LinuxCanFrame;
    use crate::frame::Direction;
    use crate::recv::Timestamp;

    fn ts(sec: i64, nsec: i64) -> Timestamp {
        Timestamp { sec, nsec }
    }

    #[test]
    fn canutils_format_basic() {
        let frame = CanFrame {
            sock_id: 0,
            timestamp: Timestamp {
                sec: 1616161616,
                nsec: 123000,
            },
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(0x18FECA00 | libc::CAN_EFF_FLAG, &[0xAA, 0xBB, 0x00, 0x11]),
        };
        let names = vec!["can0".to_string()];
        let fmt = CanutilsFormatter::new(names);
        let mut buf = Vec::new();
        fmt.format(&frame, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(1616161616.000123) can0 18FECA00#AABB0011\n"
        );
    }

    #[test]
    fn canutils_format_empty_data() {
        let frame = CanFrame {
            sock_id: 1,
            timestamp: Timestamp { sec: 0, nsec: 0 },
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(0x123 | libc::CAN_EFF_FLAG, &[]),
        };
        let names = vec!["vcan0".to_string(), "vcan1".to_string()];
        let fmt = CanutilsFormatter::new(names);
        let mut buf = Vec::new();
        fmt.format(&frame, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(0.000000) vcan1 00000123#\n"
        );
    }

    #[test]
    fn canutils_format_error_frame_keeps_err_flag() {
        let frame = CanFrame {
            sock_id: 0,
            timestamp: Timestamp { sec: 1, nsec: 0 },
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(libc::CAN_ERR_FLAG | 0x40, &[0, 0, 0, 0, 0, 0, 0, 0]),
        };
        let names = vec!["can0".to_string()];
        let fmt = CanutilsFormatter::new(names);
        let mut buf = Vec::new();
        fmt.format(&frame, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(1.000000) can0 20000040#0000000000000000\n"
        );
    }

    #[test]
    fn canutils_format_standard_frame_uses_three_digits() {
        let frame = CanFrame {
            sock_id: 0,
            timestamp: Timestamp { sec: 0, nsec: 0 },
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(0x123, &[0xAB]),
        };
        let names = vec!["can0".to_string()];
        let fmt = CanutilsFormatter::new(names);
        let mut buf = Vec::new();
        fmt.format(&frame, &mut buf);
        assert_eq!(String::from_utf8(buf).unwrap(), "(0.000000) can0 123#AB\n");
    }

    #[test]
    fn absolute_returns_frame_time_unchanged() {
        let mut clock = TimestampClock::new(TimestampMode::Absolute);
        assert_eq!(
            clock.resolve(ts(1_700_000_000, 123_456_000)),
            DisplayTimestamp::Absolute(ts(1_700_000_000, 123_456_000))
        );
    }

    #[test]
    fn zero_is_elapsed_since_first_frame() {
        let mut clock = TimestampClock::new(TimestampMode::Zero);
        assert_eq!(
            clock.resolve(ts(100, 0)),
            DisplayTimestamp::Relative(Duration::ZERO)
        );
        assert_eq!(
            clock.resolve(ts(100, 250_000_000)),
            DisplayTimestamp::Relative(Duration::from_micros(250_000))
        );
        // Relative to the first frame (100), not the previous one.
        assert_eq!(
            clock.resolve(ts(105, 0)),
            DisplayTimestamp::Relative(Duration::from_secs(5))
        );
    }

    #[test]
    fn delta_is_gap_between_consecutive_frames() {
        let mut clock = TimestampClock::new(TimestampMode::Delta);
        assert_eq!(
            clock.resolve(ts(100, 0)),
            DisplayTimestamp::Relative(Duration::ZERO)
        );
        assert_eq!(
            clock.resolve(ts(100, 250_000_000)),
            DisplayTimestamp::Relative(Duration::from_micros(250_000))
        );
        // Relative to the previous frame (100.25), not the first one.
        assert_eq!(
            clock.resolve(ts(102, 0)),
            DisplayTimestamp::Relative(Duration::from_micros(1_750_000))
        );
    }

    #[test]
    fn relative_clamps_backward_clock_to_zero() {
        let mut clock = TimestampClock::new(TimestampMode::Delta);
        clock.resolve(ts(100, 0));
        assert_eq!(
            clock.resolve(ts(99, 0)),
            DisplayTimestamp::Relative(Duration::ZERO)
        );
    }
}
