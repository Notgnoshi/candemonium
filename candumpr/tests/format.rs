use std::os::unix::io::AsFd;
use std::process::{Command, Stdio};
use std::time::Duration;

use candumpr::can::{self, LinuxCanFrame};
use pretty_assertions::assert_eq;
use vcan_fixture::VcanHarness;

#[ctor::ctor]
fn setup() {
    tracing_subscriber::fmt().with_test_writer().init();
    vcan_fixture::enter_namespace();
}

fn run_and_log_one_frame(extra_args: &[&str]) -> (String, Vec<String>) {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = vcans.names()[0].clone();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_candumpr"));
    cmd.arg("--log-level=TRACE");
    for arg in extra_args {
        cmd.arg(arg);
    }
    let child = cmd
        .arg(&iface)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(200));

    let tx = can::open_can_raw_blocking(&iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();

    std::thread::sleep(Duration::from_millis(300));
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    print!("{stdout}");
    let lines = stdout.lines().map(str::to_string).collect();
    (iface, lines)
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn console_format_with_zero_timestamp_is_deterministic() {
    let (iface, lines) =
        run_and_log_one_frame(&["--format", "candump-console", "--timestamp", "zero"]);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], format!("(000.000000) {iface} 123 [1] AB"));
}
