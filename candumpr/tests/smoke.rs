use std::ffi::CString;
use std::{io, mem, ptr};

use vcan_fixture::VcanHarness;

#[ctor::ctor]
fn setup() {
    vcan_fixture::enter_namespace();
}

/// Verify that we can receive CAN frames from multiple vcan interfaces. This is the basic
/// operation that candumpr needs to perform.
#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn recv_frames_from_multiple_interfaces() {
    let vcans = VcanHarness::new(2).unwrap();

    #[repr(C)]
    struct CanFrame {
        can_id: u32,
        len: u8,
        _pad: u8,
        _res0: u8,
        _len8_dlc: u8,
        data: [u8; 8],
    }

    // Send one frame on each interface and verify we can receive it on a separate socket.
    for (i, iface) in vcans.names().iter().enumerate() {
        let name_c = CString::new(iface.as_str()).unwrap();
        let ifindex = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
        assert!(ifindex > 0, "interface {iface} not found");

        let mut addr: libc::sockaddr_can = unsafe { mem::zeroed() };
        addr.can_family = libc::AF_CAN as u16;
        addr.can_ifindex = ifindex as i32;

        // Separate tx and rx sockets. vcan delivers frames to all sockets bound to the
        // interface except the sender.
        let tx = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        let rx = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        assert!(tx >= 0, "tx socket: {}", io::Error::last_os_error());
        assert!(rx >= 0, "rx socket: {}", io::Error::last_os_error());

        for fd in [tx, rx] {
            let ret = unsafe {
                libc::bind(
                    fd,
                    ptr::from_ref(&addr).cast::<libc::sockaddr>(),
                    mem::size_of::<libc::sockaddr_can>() as u32,
                )
            };
            assert_eq!(ret, 0, "bind to {iface}: {}", io::Error::last_os_error());
        }

        // Use a different source address per interface so we can verify which frame we got.
        let sa = i as u8;
        let frame = CanFrame {
            can_id: 0x18FECA00 | (sa as u32) | libc::CAN_EFF_FLAG,
            len: 3,
            _pad: 0,
            _res0: 0,
            _len8_dlc: 0,
            data: [0xAA, 0xBB, sa, 0, 0, 0, 0, 0],
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
        assert_eq!(
            recv_frame.can_id,
            0x18FECA00 | (sa as u32) | libc::CAN_EFF_FLAG
        );
        assert_eq!(&recv_frame.data[..3], &[0xAA, 0xBB, sa]);

        unsafe {
            libc::close(tx);
            libc::close(rx);
        }
    }
}
