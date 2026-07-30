use std::io::{BufRead, BufReader};
use std::os::unix::io::AsFd;
use std::time::{Duration, Instant};

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

/// When the stdout consumer goes away (`candumpr can0 | head`), candumpr must notice the broken
/// pipe on its next write and exit cleanly on its own, without being signalled.
#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn exits_cleanly_when_stdout_closes() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];

    // TRACE log level fills up the stderr buf and prevents EPIPE error from being raised
    let mut child = tool!("candumpr", "INFO").arg(iface).spawn_piped().unwrap();

    // Give the process time to set up io_uring and start receiving.
    std::thread::sleep(Duration::from_millis(200));

    let tx = can::open_can_raw_blocking(iface).unwrap();
    let frame = LinuxCanFrame::new(0x18FECA00 | libc::CAN_EFF_FLAG, &[0xAA, 0xBB]);

    // Prove the pipe is live: send one frame and read the line it produces.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    can::send_frame(tx.as_fd(), &frame).unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(
        line.ends_with('\n'),
        "expected a full output line: {line:?}"
    );

    // Close the read end, like `head` exiting. The next batch write hits EPIPE.
    drop(stdout);
    can::send_frame(tx.as_fd(), &frame).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "candumpr did not exit after stdout closed. stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let output = child.wait_with_output().unwrap();
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        status.success(),
        "expected clean exit after stdout closed, got {status}"
    );
}
