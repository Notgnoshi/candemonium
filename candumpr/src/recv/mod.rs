#[cfg(any(test, feature = "bench"))]
pub mod backends;
pub mod netlink;
pub mod receiver;

/// Per-frame metadata delivered alongside the CAN frame.
///
/// Backends that support ancillary data (recvmsg-based) populate these fields
/// from cmsg. Backends using plain read() pass [Default::default].
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameMeta {
    /// Receive timestamp as seconds + nanoseconds since the Unix epoch.
    /// From `SCM_TIMESTAMPING` (hardware) or `SO_TIMESTAMPNS` (software).
    /// None if the backend does not support timestamps.
    pub timestamp: Option<Timestamp>,

    /// Cumulative number of frames dropped by the kernel on this socket
    /// since the socket was created. From `SO_RXQ_OVFL` cmsg.
    /// None if the backend does not support drop notification.
    pub drops: Option<u32>,
}

/// A timestamp with seconds and nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub sec: i64,
    pub nsec: i64,
}
