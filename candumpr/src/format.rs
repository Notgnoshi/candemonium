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
    fn format(&mut self, frame: &CanFrame, buf: &mut Vec<u8>);

    /// Optional header bytes written once at the start of each output stream, before any frames.
    ///
    /// Used by formats with a file-level header (e.g. PCAP).
    fn header(&self) -> Option<&[u8]> {
        None
    }
}

/// The display CAN ID (with flags masked appropriately) and the zero-padded hex width to render it.
///
/// Error frames keep the error flag and render at width 8; extended frames render at width 8;
/// standard frames render at width 3.
fn canid_and_width(can_id: u32) -> (u32, usize) {
    if can_id & libc::CAN_ERR_FLAG != 0 {
        (can_id & (libc::CAN_ERR_MASK | libc::CAN_ERR_FLAG), 8)
    } else if can_id & libc::CAN_EFF_FLAG != 0 {
        (can_id & libc::CAN_EFF_MASK, 8)
    } else {
        (can_id & libc::CAN_SFF_MASK, 3)
    }
}

/// Formats frames in the can-utils candump file (log) format.
///
/// Output format: `(TIMESTAMP) IFACE CANID#DATA\n`, e.g. `(1616161616.123456) can0 18FECA00#AABB0011`.
/// The timestamp is rendered according to the configured [TimestampMode].
pub struct CanutilsFileFormatter {
    iface_names: Vec<String>,
    clock: TimestampClock,
}

impl CanutilsFileFormatter {
    pub fn new(iface_names: Vec<String>, timestamp: TimestampMode) -> Self {
        Self {
            iface_names,
            clock: TimestampClock::new(timestamp),
        }
    }
}

impl Formatter for CanutilsFileFormatter {
    fn format(&mut self, frame: &CanFrame, buf: &mut Vec<u8>) {
        let display = self.clock.resolve(frame.timestamp);
        write_timestamp(buf, display);

        let iface = &self.iface_names[frame.sock_id];
        let (id, width) = canid_and_width(frame.raw.can_id);
        write!(buf, "{iface} {id:0width$X}#").unwrap();
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

    fn frame(sock_id: usize, timestamp: Timestamp, can_id: u32, data: &[u8]) -> CanFrame {
        CanFrame {
            sock_id,
            timestamp,
            direction: Direction::Rx,
            raw: LinuxCanFrame::new(can_id, data),
        }
    }

    #[test]
    fn canutils_format_basic() {
        let mut fmt = CanutilsFileFormatter::new(vec!["can0".to_string()], TimestampMode::Absolute);
        let mut buf = Vec::new();
        fmt.format(
            &frame(
                0,
                ts(1616161616, 123000),
                0x18FECA00 | libc::CAN_EFF_FLAG,
                &[0xAA, 0xBB, 0x00, 0x11],
            ),
            &mut buf,
        );
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(1616161616.000123) can0 18FECA00#AABB0011\n"
        );
    }

    #[test]
    fn canutils_format_empty_data() {
        let mut fmt = CanutilsFileFormatter::new(
            vec!["vcan0".to_string(), "vcan1".to_string()],
            TimestampMode::Absolute,
        );
        let mut buf = Vec::new();
        fmt.format(
            &frame(1, ts(0, 0), 0x123 | libc::CAN_EFF_FLAG, &[]),
            &mut buf,
        );
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(0.000000) vcan1 00000123#\n"
        );
    }

    #[test]
    fn canutils_format_error_frame_keeps_err_flag() {
        let mut fmt = CanutilsFileFormatter::new(vec!["can0".to_string()], TimestampMode::Absolute);
        let mut buf = Vec::new();
        fmt.format(
            &frame(0, ts(1, 0), libc::CAN_ERR_FLAG | 0x40, &[0; 8]),
            &mut buf,
        );
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(1.000000) can0 20000040#0000000000000000\n"
        );
    }

    #[test]
    fn canutils_format_standard_frame_uses_three_digits() {
        let mut fmt = CanutilsFileFormatter::new(vec!["can0".to_string()], TimestampMode::Absolute);
        let mut buf = Vec::new();
        fmt.format(&frame(0, ts(0, 0), 0x123, &[0xAB]), &mut buf);
        assert_eq!(String::from_utf8(buf).unwrap(), "(0.000000) can0 123#AB\n");
    }

    #[test]
    fn canutils_format_delta_timestamps() {
        let mut fmt = CanutilsFileFormatter::new(vec!["can0".to_string()], TimestampMode::Delta);
        let mut buf = Vec::new();
        fmt.format(&frame(0, ts(100, 0), 0x123, &[0xAB]), &mut buf);
        fmt.format(&frame(0, ts(100, 250_000_000), 0x123, &[0xAB]), &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(000.000000) can0 123#AB\n(000.250000) can0 123#AB\n"
        );
    }

    #[test]
    fn canutils_format_zero_timestamps() {
        let mut fmt = CanutilsFileFormatter::new(vec!["can0".to_string()], TimestampMode::Zero);
        let mut buf = Vec::new();
        fmt.format(&frame(0, ts(100, 0), 0x123, &[0xAB]), &mut buf);
        fmt.format(&frame(0, ts(105, 0), 0x123, &[0xAB]), &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "(000.000000) can0 123#AB\n(005.000000) can0 123#AB\n"
        );
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
