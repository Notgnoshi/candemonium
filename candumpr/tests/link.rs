use std::os::unix::io::AsFd;
use std::time::Duration;

use candumpr::can::{self, LinuxCanFrame};
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
fn rides_through_link_down_and_resumes() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = vcans.names()[0].clone();

    let child = tool!("candumpr", "INFO").arg(&iface).spawn_piped().unwrap();

    // Let the netlink monitor connect and dump the initial (up) state.
    std::thread::sleep(Duration::from_millis(400));

    vcans.set_down(&iface).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    vcans.set_up(&iface).unwrap();
    std::thread::sleep(Duration::from_millis(250));

    // A frame sent after the link returns proves the receiver survived ENETDOWN and resumed.
    let tx = can::open_can_raw_blocking(&iface).unwrap();
    can::send_frame(
        tx.as_fd(),
        &LinuxCanFrame::new(0x18FECA00 | libc::CAN_EFF_FLAG, &[0xDE, 0xAD]),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(250));

    child.signal(libc::SIGINT).unwrap();
    let output = child.captured_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stderr.contains("interface link down"),
        "expected a link-down log line, got:\n{stderr}"
    );
    assert!(
        stderr.contains("interface link up"),
        "expected a link-up log line, got:\n{stderr}"
    );
    assert!(
        stdout.contains("18FECA00#DEAD"),
        "expected a frame received after the link returned, got:\n{stdout}"
    );
}
