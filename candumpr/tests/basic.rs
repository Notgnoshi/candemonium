use std::os::unix::io::AsFd;
use std::time::Duration;

use candumpr::can::{self, LinuxCanFrame};
use pretty_assertions::assert_eq;
use vcan_fixture::VcanHarness;
use vcan_fixture::prelude::*;

#[ctor::ctor]
fn setup() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(true)
        .init();
    vcan_fixture::enter_namespace();
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn canutils_stdout_output() {
    let vcans = VcanHarness::new(2).unwrap();
    let iface0 = &vcans.names()[0];
    let iface1 = &vcans.names()[1];

    let child = tool!("candumpr")
        .arg("--no-request-address-claims")
        .arg(iface0)
        .arg(iface1)
        .spawn_piped()
        .unwrap();

    // Give the process time to set up io_uring and start receiving.
    std::thread::sleep(Duration::from_millis(200));

    // Send known frames on each interface.
    let tx0 = can::open_can_raw_blocking(iface0).unwrap();
    let tx1 = can::open_can_raw_blocking(iface1).unwrap();

    can::send_frame(
        tx0.as_fd(),
        &LinuxCanFrame::new(0x18FECA00 | libc::CAN_EFF_FLAG, &[0xAA, 0xBB]),
    )
    .unwrap();
    can::send_frame(
        tx1.as_fd(),
        &LinuxCanFrame::new(0x123 | libc::CAN_EFF_FLAG, &[0x01, 0x02, 0x03]),
    )
    .unwrap();

    // Give frames time to be received and written.
    std::thread::sleep(Duration::from_millis(300));

    // Send SIGINT to shut down gracefully.
    child.signal(libc::SIGINT).unwrap();

    let output = child.captured_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Strip the dynamic timestamp prefix from each line. The format is:
    // (SECONDS.MICROSECONDS) IFACE CANID#DATA
    // Everything after ") " is deterministic.
    let lines: Vec<&str> = stdout
        .lines()
        .map(|line| line.split_once(") ").unwrap().1)
        .collect();

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], format!("{iface0} 18FECA00#AABB"));
    assert_eq!(lines[1], format!("{iface1} 00000123#010203"));
}
