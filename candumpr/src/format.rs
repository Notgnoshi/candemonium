use std::io::Write;

use crate::frame::CanFrame;

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
}
