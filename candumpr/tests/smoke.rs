use std::os::unix::io::AsFd;

use candumpr::can::{self, CanFrame};
use vcan_fixture::VcanHarness;

#[ctor::ctor]
fn setup() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(true)
        .init();
    vcan_fixture::enter_namespace();
}

/// Verify that we can receive CAN frames from multiple vcan interfaces. This is the basic
/// operation that candumpr needs to perform.
#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn recv_frames_from_multiple_interfaces() {
    let vcans = VcanHarness::new(2).unwrap();

    // Send one frame on each interface and verify we can receive it on a separate socket.
    for (i, iface) in vcans.names().iter().enumerate() {
        let tx = can::open_can_raw_blocking(iface).unwrap();
        let rx = can::open_can_raw_blocking(iface).unwrap();

        // Use a different source address per interface so we can verify which frame we got.
        let sa = i as u8;
        let frame = CanFrame::new(
            0x18FECA00 | (sa as u32) | libc::CAN_EFF_FLAG,
            &[0xAA, 0xBB, sa],
        );

        can::send_frame(tx.as_fd(), &frame).unwrap();

        let mut recv_frame = CanFrame::default();
        let read = unsafe {
            libc::read(
                std::os::unix::io::AsRawFd::as_raw_fd(&rx),
                std::ptr::from_mut(&mut recv_frame).cast::<libc::c_void>(),
                can::FRAME_SIZE,
            )
        };
        assert_eq!(read as usize, can::FRAME_SIZE);
        assert_eq!(
            recv_frame.can_id,
            0x18FECA00 | (sa as u32) | libc::CAN_EFF_FLAG
        );
        assert_eq!(&recv_frame.data[..3], &[0xAA, 0xBB, sa]);
    }
}
