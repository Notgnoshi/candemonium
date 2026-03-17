//! Test fixture for creating isolated vcan interfaces inside user + network namespaces.
//!
//! # Overview
//!
//! Tests that need CAN sockets require vcan interfaces, which normally need root to create. This
//! crate solves that by entering a user + network namespace via `unshare(2)`, which grants
//! `CAP_NET_ADMIN` without real root privileges. Each test process gets its own isolated namespace
//! with its own vcan interfaces.
//!
//! # Usage
//!
//! Namespace entry must happen before the test harness spawns threads. Use a `ctor` constructor:
//!
//! ```ignore
//! #[ctor::ctor]
//! fn setup() {
//!     vcan_fixture::enter_namespace();
//! }
//!
//! #[test]
//! fn my_can_test() {
//!     let vcans = vcan_fixture::VcanHarness::new(2).unwrap();
//!     // vcans.names() -> ["vcan0", "vcan1"]
//! }
//! ```
//!
//! # Prerequisites
//!
//! The `vcan` kernel module must be loaded on the host before tests run.
//!
//! ## Fedora 42+
//!
//! ```ignore
//! sudo modprobe vcan
//! ```
//!
//! ## Ubuntu 24.04
//!
//! ```ignore
//! # The vcan module is in a separate package
//! sudo apt-get install -y linux-modules-extra-"$(uname -r)"
//! sudo modprobe vcan
//!
//! # Ubuntu 24.04 restricts unprivileged user namespaces via AppArmor
//! sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
//! ```
//!
//! In Ubuntu 25.10+ `linux-modules-extra` will get merged back into `linux-modules` and be
//! available by default.

pub mod bench;
mod netlink;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{fs, io};

static IN_NAMESPACE: AtomicBool = AtomicBool::new(false);
static NEXT_VCAN_ID: AtomicU32 = AtomicU32::new(0);

/// Enter a new user + network namespace.
///
/// Must be called while the process is single-threaded. The intended call site is a `ctor`
/// constructor that runs before `main()`.
///
/// If namespace creation fails (unsupported kernel, AppArmor restrictions, etc.), this prints
/// a diagnostic to stderr and returns without entering a namespace. Callers should check
/// [in_namespace] before attempting to create interfaces.
pub fn enter_namespace() -> bool {
    // Save UID/GID before unshare, since they become unmapped (65534) in the new namespace
    // until we write the mappings.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let ret = unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        tracing::error!("unshare(CLONE_NEWUSER | CLONE_NEWNET) failed: {err}");
        return false;
    }

    if let Err(e) = write_id_mappings(uid, gid) {
        tracing::error!("failed to write id mappings: {e}");
        return false;
    }

    IN_NAMESPACE.store(true, Ordering::Release);
    true
}

/// Returns true if the process has entered an isolated network namespace.
pub fn in_namespace() -> bool {
    IN_NAMESPACE.load(Ordering::Acquire)
}

/// Returns true if the vcan kernel module appears to be loaded on the host.
pub fn is_vcan_available() -> bool {
    fs::read_to_string("/proc/modules")
        .map(|s| s.lines().any(|line| line.starts_with("vcan ")))
        .unwrap_or(false)
}

/// Returns true if the test environment is fully set up: inside a namespace with the vcan
/// module loaded.
pub fn vcan_available() -> bool {
    in_namespace() && is_vcan_available()
}

/// A set of vcan interfaces created for a single test.
///
/// Interface names use a global atomic counter for uniqueness (`vcan0`, `vcan1`, ...), so
/// parallel tests within the same process do not collide. Interfaces are deleted on drop.
pub struct VcanHarness {
    names: Vec<String>,
}

impl VcanHarness {
    /// Create `count` vcan interfaces.
    ///
    /// Requires [enter_namespace] to have succeeded. Each interface is created with a unique
    /// name and brought up before this returns.
    pub fn new(count: usize) -> eyre::Result<Self> {
        let mut names = Vec::with_capacity(count);
        for _ in 0..count {
            let id = NEXT_VCAN_ID.fetch_add(1, Ordering::Relaxed);
            let name = format!("vcan{id}");
            tracing::info!(link = %name, "creating vcan link");
            netlink::create_vcan(&name)?;
            names.push(name);
        }
        Ok(VcanHarness { names })
    }

    /// The interface names (e.g., `["vcan0", "vcan1"]`).
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Bring an interface up by name.
    pub fn set_up(&self, name: &str) -> eyre::Result<()> {
        tracing::info!(link = %name, "setting link up");
        netlink::set_link_up(name)
    }

    /// Bring an interface down by name.
    pub fn set_down(&self, name: &str) -> eyre::Result<()> {
        tracing::info!(link = %name, "setting link down");
        netlink::set_link_down(name)
    }
}

impl Drop for VcanHarness {
    fn drop(&mut self) {
        for name in &self.names {
            if let Err(e) = netlink::delete_vcan(name) {
                tracing::error!(link = %name, "failed to delete vcan: {e}");
            }
        }
    }
}

/// Map UID/GID 0 inside the namespace to the real UID/GID outside.
fn write_id_mappings(uid: u32, gid: u32) -> eyre::Result<()> {
    // Must deny setgroups before writing gid_map as an unprivileged user (since Linux 3.19).
    fs::write("/proc/self/setgroups", "deny")?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::{mem, ptr};

    use super::*;

    #[ctor::ctor]
    fn setup() {
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_ansi(true)
            .init();
        enter_namespace();
    }

    #[test]
    #[cfg_attr(feature = "ci", ignore = "requires vcan")]
    fn create_vcan_in_namespace() {
        let vcans = VcanHarness::new(2).unwrap();
        assert_eq!(vcans.names().len(), 2);

        for iface in vcans.names() {
            let name_c = CString::new(iface.as_str()).unwrap();
            let ifindex = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
            assert!(ifindex > 0, "interface {iface} not found");

            // Verify we can open and bind a CAN socket.
            let fd = unsafe {
                libc::socket(
                    libc::PF_CAN,
                    libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                    libc::CAN_RAW,
                )
            };
            assert!(fd >= 0, "socket: {}", io::Error::last_os_error());

            let mut addr: libc::sockaddr_can = unsafe { mem::zeroed() };
            addr.can_family = libc::AF_CAN as u16;
            addr.can_ifindex = ifindex as i32;
            let ret = unsafe {
                libc::bind(
                    fd,
                    ptr::from_ref(&addr).cast::<libc::sockaddr>(),
                    mem::size_of::<libc::sockaddr_can>() as u32,
                )
            };
            assert_eq!(ret, 0, "bind to {iface}: {}", io::Error::last_os_error());
            unsafe { libc::close(fd) };
        }
    }

    #[test]
    #[cfg_attr(feature = "ci", ignore = "requires vcan")]
    fn send_and_recv_frame() {
        let vcans = VcanHarness::new(1).unwrap();
        let iface = &vcans.names()[0];
        let name_c = CString::new(iface.as_str()).unwrap();
        let ifindex = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
        assert!(ifindex > 0);

        let tx = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        let rx = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        assert!(tx >= 0);
        assert!(rx >= 0);

        let mut addr: libc::sockaddr_can = unsafe { mem::zeroed() };
        addr.can_family = libc::AF_CAN as u16;
        addr.can_ifindex = ifindex as i32;
        for fd in [tx, rx] {
            let ret = unsafe {
                libc::bind(
                    fd,
                    ptr::from_ref(&addr).cast::<libc::sockaddr>(),
                    mem::size_of::<libc::sockaddr_can>() as u32,
                )
            };
            assert_eq!(ret, 0);
        }

        #[repr(C)]
        struct CanFrame {
            can_id: u32,
            len: u8,
            __pad: u8,
            __res0: u8,
            __len8_dlc: u8,
            data: [u8; 8],
        }

        let frame = CanFrame {
            can_id: 0x18FECA00 | libc::CAN_EFF_FLAG,
            len: 8,
            __pad: 0,
            __res0: 0,
            __len8_dlc: 0,
            data: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
        };

        let written = unsafe {
            libc::write(
                tx,
                ptr::from_ref(&frame).cast::<libc::c_void>(),
                mem::size_of::<CanFrame>(),
            )
        };
        assert_eq!(written as usize, mem::size_of::<CanFrame>());

        let mut recv_frame: CanFrame = unsafe { mem::zeroed() };
        let read = unsafe {
            libc::read(
                rx,
                ptr::from_mut(&mut recv_frame).cast::<libc::c_void>(),
                mem::size_of::<CanFrame>(),
            )
        };
        assert_eq!(read as usize, mem::size_of::<CanFrame>());
        assert_eq!(recv_frame.can_id, 0x18FECA00 | libc::CAN_EFF_FLAG);
        assert_eq!(recv_frame.len, 8);
        assert_eq!(
            recv_frame.data,
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]
        );

        unsafe {
            libc::close(tx);
            libc::close(rx);
        }
    }
}
