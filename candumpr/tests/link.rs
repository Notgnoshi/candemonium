use std::os::unix::io::AsFd;
use std::process::{Command, Stdio};
use std::time::Duration;

use candumpr::can::{self, LinuxCanFrame};
use vcan_fixture::VcanHarness;

#[ctor::ctor]
fn setup() {
    tracing_subscriber::fmt().with_test_writer().init();
    vcan_fixture::enter_namespace();
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn rides_through_link_down_and_resumes() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = vcans.names()[0].clone();

    let child = Command::new(env!("CARGO_BIN_EXE_candumpr"))
        .arg("--log-level=INFO")
        .arg(&iface)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

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

    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprint!("{stderr}");
    print!("{stdout}");

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
