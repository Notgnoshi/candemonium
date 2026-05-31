use crate::can::LinuxCanFrame;
use crate::recv::Timestamp;

/// Direction of a CAN frame relative to the local device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Received from the bus (another device sent it).
    Rx,
    /// Loopback of a frame this device transmitted.
    Tx,
}

/// A CAN frame enriched with receive metadata for the write pipeline.
///
/// Sent from the receiver thread to the writer thread over the SPSC channel.
#[derive(Clone, Debug)]
pub struct CanFrame {
    /// Source interface: its position in the configured sockets vector
    pub sock_id: usize,
    /// Receive timestamp.
    pub timestamp: Timestamp,
    /// Whether this was received from the bus or is a local TX loopback.
    pub direction: Direction,
    /// The raw CAN frame.
    pub raw: LinuxCanFrame,
}
