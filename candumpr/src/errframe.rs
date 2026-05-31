//! Parsing and decoding of SocketCAN error frames (linux/can/error.h).
use std::fmt;

use crate::can::LinuxCanFrame;

// Error-class bits in `can_id` (masked by CAN_ERR_MASK). libc exposes only CAN_ERR_MASK.
const CAN_ERR_TX_TIMEOUT: u32 = 0x0000_0001;
const CAN_ERR_LOSTARB: u32 = 0x0000_0002;
const CAN_ERR_CRTL: u32 = 0x0000_0004;
const CAN_ERR_PROT: u32 = 0x0000_0008;
const CAN_ERR_TRX: u32 = 0x0000_0010;
const CAN_ERR_ACK: u32 = 0x0000_0020;
const CAN_ERR_BUSOFF: u32 = 0x0000_0040;
const CAN_ERR_BUSERROR: u32 = 0x0000_0080;
const CAN_ERR_RESTARTED: u32 = 0x0000_0100;
const CAN_ERR_CNT: u32 = 0x0000_0200;

// Controller status detail in data[1] (CAN_ERR_CRTL).
const CAN_ERR_CRTL_RX_OVERFLOW: u8 = 0x01;
const CAN_ERR_CRTL_TX_OVERFLOW: u8 = 0x02;
const CAN_ERR_CRTL_RX_WARNING: u8 = 0x04;
const CAN_ERR_CRTL_TX_WARNING: u8 = 0x08;
const CAN_ERR_CRTL_RX_PASSIVE: u8 = 0x10;
const CAN_ERR_CRTL_TX_PASSIVE: u8 = 0x20;
const CAN_ERR_CRTL_ACTIVE: u8 = 0x40;

// Protocol violation type detail in data[2] (CAN_ERR_PROT).
const CAN_ERR_PROT_BIT: u8 = 0x01;
const CAN_ERR_PROT_FORM: u8 = 0x02;
const CAN_ERR_PROT_STUFF: u8 = 0x04;
const CAN_ERR_PROT_BIT0: u8 = 0x08;
const CAN_ERR_PROT_BIT1: u8 = 0x10;
const CAN_ERR_PROT_OVERLOAD: u8 = 0x20;
const CAN_ERR_PROT_ACTIVE: u8 = 0x40;
const CAN_ERR_PROT_TX: u8 = 0x80;

// Protocol violation location, indexed directly by data[3] (CAN_ERR_PROT).
const PROT_LOCATIONS: [&str; 32] = [
    "unspecified",            // 0x00
    "unspecified",            // 0x01
    "id.28-to-id.21",         // 0x02
    "start-of-frame",         // 0x03
    "bit-srtr",               // 0x04
    "bit-ide",                // 0x05
    "id.20-to-id.18",         // 0x06
    "id.17-to-id.13",         // 0x07
    "crc-sequence",           // 0x08
    "reserved-bit-0",         // 0x09
    "data-field",             // 0x0A
    "data-length-code",       // 0x0B
    "bit-rtr",                // 0x0C
    "reserved-bit-1",         // 0x0D
    "id.4-to-id.0",           // 0x0E
    "id.12-to-id.5",          // 0x0F
    "unspecified",            // 0x10
    "active-error-flag",      // 0x11
    "intermission",           // 0x12
    "tolerate-dominant-bits", // 0x13
    "unspecified",            // 0x14
    "unspecified",            // 0x15
    "passive-error-flag",     // 0x16
    "error-delimiter",        // 0x17
    "crc-delimiter",          // 0x18
    "acknowledge-slot",       // 0x19
    "end-of-frame",           // 0x1A
    "acknowledge-delimiter",  // 0x1B
    "overload-flag",          // 0x1C
    "unspecified",            // 0x1D
    "unspecified",            // 0x1E
    "unspecified",            // 0x1F
];

/// The error state of a CAN controller, ordered from healthy (error-active) to faulted (bus-off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BusState {
    /// ErrorActive means we're Active in the error state machine, not that there's an active error.
    #[default]
    ErrorActive,
    ErrorWarning,
    ErrorPassive,
    BusOff,
}

impl fmt::Display for BusState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BusState::ErrorActive => "error-active",
            BusState::ErrorWarning => "error-warning",
            BusState::ErrorPassive => "error-passive",
            BusState::BusOff => "bus-off",
        })
    }
}

/// A SocketCAN error frame. Construct with [parse](ErrorFrame::parse);
pub struct ErrorFrame<'a>(&'a LinuxCanFrame);

impl<'a> ErrorFrame<'a> {
    /// An error-frame view of `frame`, or `None` if it is an ordinary data frame.
    pub fn parse(frame: &'a LinuxCanFrame) -> Option<Self> {
        (frame.can_id & libc::CAN_ERR_FLAG != 0).then_some(ErrorFrame(frame))
    }

    /// The controller state this frame reports, or `None` if it carries no controller-state change.
    pub fn bus_state(&self) -> Option<BusState> {
        let class = self.0.can_id & libc::CAN_ERR_MASK;
        if class & CAN_ERR_BUSOFF != 0 {
            Some(BusState::BusOff)
        } else if class & CAN_ERR_RESTARTED != 0 {
            Some(BusState::ErrorActive)
        } else if class & CAN_ERR_CRTL != 0 {
            let ctrl = self.0.data[1];
            if ctrl & (CAN_ERR_CRTL_RX_PASSIVE | CAN_ERR_CRTL_TX_PASSIVE) != 0 {
                Some(BusState::ErrorPassive)
            } else if ctrl & (CAN_ERR_CRTL_RX_WARNING | CAN_ERR_CRTL_TX_WARNING) != 0 {
                Some(BusState::ErrorWarning)
            } else if ctrl & CAN_ERR_CRTL_ACTIVE != 0 {
                Some(BusState::ErrorActive)
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl fmt::Display for ErrorFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = self.0.can_id & libc::CAN_ERR_MASK;
        let data = &self.0.data;
        let mut tokens: Vec<String> = Vec::new();

        if class & CAN_ERR_TX_TIMEOUT != 0 {
            tokens.push("tx-timeout".to_string());
        }
        if class & CAN_ERR_LOSTARB != 0 {
            tokens.push(format!("lost-arbitration{{at bit {}}}", data[0]));
        }
        if class & CAN_ERR_CRTL != 0 {
            let mut s = String::from("controller-problem{");
            s.push_str(&controller_problems(data[1]));
            s.push('}');
            tokens.push(s);
        }
        if class & CAN_ERR_PROT != 0 {
            let mut s = String::from("protocol-violation{{");
            s.push_str(&protocol_types(data[2]));
            s.push_str("}{");
            s.push_str(protocol_location(data[3]));
            s.push_str("}}");
            tokens.push(s);
        }
        if class & CAN_ERR_TRX != 0 {
            tokens.push("transceiver-status".to_string());
        }
        if class & CAN_ERR_ACK != 0 {
            tokens.push("no-acknowledgement-on-tx".to_string());
        }
        if class & CAN_ERR_BUSOFF != 0 {
            tokens.push("bus-off".to_string());
        }
        if class & CAN_ERR_BUSERROR != 0 {
            tokens.push("bus-error".to_string());
        }
        if class & CAN_ERR_RESTARTED != 0 {
            tokens.push("restarted-after-bus-off".to_string());
        }
        if class & CAN_ERR_CNT != 0 {
            let mut s = String::from("error-counter-tx-rx{{");
            s.push_str(&data[6].to_string());
            s.push_str("}{");
            s.push_str(&data[7].to_string());
            s.push_str("}}");
            tokens.push(s);
        }

        write!(f, "{}", tokens.join(","))
    }
}

fn controller_problems(byte: u8) -> String {
    let mut tokens = Vec::new();
    for (bit, name) in [
        (CAN_ERR_CRTL_RX_OVERFLOW, "rx-overflow"),
        (CAN_ERR_CRTL_TX_OVERFLOW, "tx-overflow"),
        (CAN_ERR_CRTL_RX_WARNING, "rx-error-warning"),
        (CAN_ERR_CRTL_TX_WARNING, "tx-error-warning"),
        (CAN_ERR_CRTL_RX_PASSIVE, "rx-error-passive"),
        (CAN_ERR_CRTL_TX_PASSIVE, "tx-error-passive"),
        (CAN_ERR_CRTL_ACTIVE, "back-to-error-active"),
    ] {
        if byte & bit != 0 {
            tokens.push(name);
        }
    }
    tokens.join(",")
}

fn protocol_types(byte: u8) -> String {
    let mut tokens = Vec::new();
    for (bit, name) in [
        (CAN_ERR_PROT_BIT, "single-bit-error"),
        (CAN_ERR_PROT_FORM, "frame-format-error"),
        (CAN_ERR_PROT_STUFF, "bit-stuffing-error"),
        (CAN_ERR_PROT_BIT0, "tx-dominant-bit-error"),
        (CAN_ERR_PROT_BIT1, "tx-recessive-bit-error"),
        (CAN_ERR_PROT_OVERLOAD, "bus-overload"),
        (CAN_ERR_PROT_ACTIVE, "active-error"),
        (CAN_ERR_PROT_TX, "error-on-tx"),
    ] {
        if byte & bit != 0 {
            tokens.push(name);
        }
    }
    tokens.join(",")
}

fn protocol_location(byte: u8) -> &'static str {
    PROT_LOCATIONS
        .get(usize::from(byte))
        .copied()
        .unwrap_or("unspecified")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn err_frame(class: u32, data: &[u8]) -> LinuxCanFrame {
        LinuxCanFrame::new(libc::CAN_ERR_FLAG | class, data)
    }

    fn decode(frame: &LinuxCanFrame) -> String {
        ErrorFrame::parse(frame).unwrap().to_string()
    }

    #[test]
    fn parse_rejects_data_frames() {
        let data = LinuxCanFrame::new(0x123 | libc::CAN_EFF_FLAG, &[1, 2]);
        assert!(ErrorFrame::parse(&data).is_none());
    }

    #[test]
    fn bus_state_from_discrete_classes() {
        assert_eq!(
            ErrorFrame::parse(&err_frame(CAN_ERR_BUSOFF, &[]))
                .unwrap()
                .bus_state(),
            Some(BusState::BusOff)
        );
        assert_eq!(
            ErrorFrame::parse(&err_frame(CAN_ERR_RESTARTED, &[]))
                .unwrap()
                .bus_state(),
            Some(BusState::ErrorActive)
        );
        assert_eq!(
            ErrorFrame::parse(&err_frame(CAN_ERR_CRTL, &[0, CAN_ERR_CRTL_TX_PASSIVE]))
                .unwrap()
                .bus_state(),
            Some(BusState::ErrorPassive)
        );
        // Counters alone are informational and do not yield a state.
        assert_eq!(
            ErrorFrame::parse(&err_frame(CAN_ERR_CNT, &[0, 0, 0, 0, 0, 0, 200, 0]))
                .unwrap()
                .bus_state(),
            None
        );
    }

    #[test]
    fn display_decodes_like_candump() {
        assert_eq!(decode(&err_frame(CAN_ERR_BUSOFF, &[])), "bus-off");
        assert_eq!(
            decode(&err_frame(CAN_ERR_CRTL, &[0, CAN_ERR_CRTL_TX_PASSIVE])),
            "controller-problem{tx-error-passive}"
        );
        assert_eq!(
            decode(&err_frame(CAN_ERR_PROT, &[0, 0, CAN_ERR_PROT_BIT, 0x02])),
            "protocol-violation{{single-bit-error}{id.28-to-id.21}}"
        );
        assert_eq!(
            decode(&err_frame(CAN_ERR_CNT, &[0, 0, 0, 0, 0, 0, 130, 0])),
            "error-counter-tx-rx{{130}{0}}"
        );
        // Multiple classes in one frame are comma-separated.
        assert_eq!(
            decode(&err_frame(CAN_ERR_RESTARTED | CAN_ERR_LOSTARB, &[5])),
            "lost-arbitration{at bit 5},restarted-after-bus-off"
        );
    }
}
