//! CAN frame type and raw socket helpers.
//!
//! TODO: This module probably makes sense to pull out in a shared workspace crate. That's
//! out-of-scope until more crates pop up that need it.

use std::ffi::CString;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// Linux `can_frame`: 16 bytes on the wire.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CanFrame {
    /// CAN ID with EFF/RTR/ERR flags in the upper bits.
    pub can_id: u32,
    /// Payload length in bytes (0..8).
    pub len: u8,
    pub _pad: u8,
    pub _res0: u8,
    pub _len8_dlc: u8,
    /// Payload data, 8 bytes aligned.
    pub data: [u8; 8],
}

impl CanFrame {
    /// Create a frame with the given CAN ID and data payload.
    ///
    /// The `len` field is set to `data.len()`. Panics if `data` is longer than 8 bytes.
    pub fn new(can_id: u32, data: &[u8]) -> Self {
        assert!(data.len() <= 8, "CAN frame data must be at most 8 bytes");
        let mut buf = [0u8; 8];
        buf[..data.len()].copy_from_slice(data);
        Self {
            can_id,
            len: data.len() as u8,
            _pad: 0,
            _res0: 0,
            _len8_dlc: 0,
            data: buf,
        }
    }
}

/// Size of a single CAN frame in bytes.
pub const FRAME_SIZE: usize = std::mem::size_of::<CanFrame>();
const _: () = assert!(FRAME_SIZE == 16);

/// Open a non-blocking `CAN_RAW` socket bound to the named interface.
///
/// The socket is created with `SOCK_CLOEXEC | SOCK_NONBLOCK`.
pub fn open_can_raw(ifname: &str) -> std::io::Result<OwnedFd> {
    open_can_socket(ifname, libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK)
}

/// Open a blocking `CAN_RAW` socket bound to the named interface.
///
/// The socket is created with `SOCK_CLOEXEC` only.
pub fn open_can_raw_blocking(ifname: &str) -> std::io::Result<OwnedFd> {
    open_can_socket(ifname, libc::SOCK_CLOEXEC)
}

fn open_can_socket(ifname: &str, flags: i32) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW | flags, libc::CAN_RAW) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: we just created this fd and checked it is valid.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let ifindex = ifindex_for(ifname)?;

    let mut addr: libc::sockaddr_can = unsafe { std::mem::zeroed() };
    addr.can_family = libc::AF_CAN as u16;
    addr.can_ifindex = ifindex;

    let ret = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            std::ptr::from_ref(&addr).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_can>() as u32,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(fd)
}

/// Set the kernel receive buffer size for the socket.
///
/// The kernel clamps this to `net.core.rmem_max` and then doubles it internally.
pub fn set_recv_buffer(fd: BorrowedFd<'_>, bytes: u32) -> std::io::Result<()> {
    let val = bytes as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            std::ptr::from_ref(&val).cast::<libc::c_void>(),
            std::mem::size_of::<libc::c_int>() as u32,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Send a single CAN frame on the socket.
pub fn send_frame(fd: BorrowedFd<'_>, frame: &CanFrame) -> std::io::Result<()> {
    let written = unsafe {
        libc::write(
            fd.as_raw_fd(),
            std::ptr::from_ref(frame).cast::<libc::c_void>(),
            FRAME_SIZE,
        )
    };
    if written < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if written as usize != FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short write on CAN socket",
        ));
    }
    Ok(())
}

/// Resolve an interface name to its index.
fn ifindex_for(ifname: &str) -> std::io::Result<i32> {
    let name_c = CString::new(ifname)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let idx = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
    if idx == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(idx as i32)
}
