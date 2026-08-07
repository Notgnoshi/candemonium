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

fn run_and_log_one_frame(extra_args: &[&str]) -> (String, Vec<String>) {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = vcans.names()[0].clone();

    let mut cmd = tool!("candumpr");
    for arg in extra_args {
        cmd.arg(arg);
    }
    let child = cmd.arg(&iface).spawn_piped().unwrap();

    std::thread::sleep(Duration::from_millis(200));

    let tx = can::open_can_raw_blocking(&iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();

    std::thread::sleep(Duration::from_millis(300));
    child.signal(libc::SIGINT).unwrap();
    let output = child.captured_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().map(str::to_string).collect();
    (iface, lines)
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn console_format_with_zero_timestamp_is_deterministic() {
    let (iface, lines) = run_and_log_one_frame(&[
        "--format",
        "candump-console",
        "--timestamp",
        "zero",
        "--no-request-address-claims",
    ]);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], format!("(000.000000) {iface} 123 [1] AB"));
}
