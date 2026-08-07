use std::os::unix::io::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
}

/// Wait for `count` log files to appear, and return everything in `dir` once they have.
#[track_caller]
fn wait_for_log_files(dir: &Path, count: usize) -> Vec<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let paths = entries(dir);
        if paths.len() >= count {
            return paths;
        }
        assert!(
            Instant::now() < deadline,
            "candumpr created {} of {count} log files in {dir:?}",
            paths.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn logs_one_interface_to_a_scheme_named_file() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let dir = tempfile::TempDir::new().unwrap();

    let child = tool!("candumpr")
        .args(["-l", "--timestamp", "zero"])
        .arg(iface)
        .current_dir(dir.path())
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    let tx = can::open_can_raw_blocking(iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();

    let paths = wait_for_log_files(dir.path(), 1); // panics on timeout

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    assert_eq!(paths.len(), 1, "expected exactly one log file: {paths:?}");
    let name = paths[0].file_name().unwrap().to_str().unwrap();
    // The timestamp is the frame's arrival time, so only the fixed parts are predictable.
    let prefix = format!("i0000_{iface}_");
    assert!(
        name.starts_with(&prefix) && name.ends_with(".log"),
        "expected a {prefix}*.log file, got {name}"
    );

    // Deterministic: --timestamp zero renders the first frame at 0.0, and only one frame is sent.
    assert_eq!(
        std::fs::read_to_string(&paths[0]).unwrap(),
        format!("(000.000000) {iface} 123#AB\n")
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("opened CAN socket"),
        "expected the startup line, got:\n{stderr}"
    );
    // Use two .contains() to skip over the tracing fmt ANSI sequences
    assert!(
        stderr.contains("created log file") && stderr.contains(&format!("./{name}")),
        "expected the created path logged at INFO, got:\n{stderr}"
    );
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn interleaves_multiple_interfaces_to_one_file() {
    let vcans = VcanHarness::new(2).unwrap();
    let iface1 = &vcans.names()[0];
    let iface2 = &vcans.names()[1];
    let dir = tempfile::TempDir::new().unwrap();

    let child = tool!("candumpr")
        .arg("--output=foo.bar")
        .arg(iface1)
        .arg(iface2)
        .current_dir(dir.path())
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    let tx1 = can::open_can_raw_blocking(iface1).unwrap();
    let tx2 = can::open_can_raw_blocking(iface2).unwrap();
    can::send_frame(tx1.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();
    can::send_frame(tx2.as_fd(), &LinuxCanFrame::new(0x456, &[0xCD])).unwrap();

    let paths = wait_for_log_files(dir.path(), 1); // panics on timeout

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    assert_eq!(paths.len(), 1, "expected exactly one log file: {paths:?}");
    let name = paths[0].file_name().unwrap().to_str().unwrap();
    assert_eq!(name, "foo.bar");
    let log = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(log.contains(&format!("{iface1} 123#AB\n")));
    assert!(log.contains(&format!("{iface2} 456#CD\n")));
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn logs_each_interface_to_its_own_file() {
    let vcans = VcanHarness::new(2).unwrap();
    let iface1 = &vcans.names()[0];
    let iface2 = &vcans.names()[1];
    let dir = tempfile::TempDir::new().unwrap();

    let child = tool!("candumpr")
        .args(["-l", "--timestamp", "zero", "--no-request-address-claims"])
        .arg(iface1)
        .arg(iface2)
        .current_dir(dir.path())
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    let tx1 = can::open_can_raw_blocking(iface1).unwrap();
    let tx2 = can::open_can_raw_blocking(iface2).unwrap();
    can::send_frame(tx1.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();
    // Separate the two frames in time, to attempt to prove that each interface has its own relative
    // timestamping.
    std::thread::sleep(Duration::from_millis(150));
    can::send_frame(tx2.as_fd(), &LinuxCanFrame::new(0x456, &[0xCD])).unwrap();

    let paths = wait_for_log_files(dir.path(), 2); // panics on timeout

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    assert_eq!(paths.len(), 2, "expected exactly two log files: {paths:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");

    for (iface, frame) in [(iface1, "123#AB"), (iface2, "456#CD")] {
        let prefix = format!("i0000_{iface}_");
        let path = paths
            .iter()
            .find(|p| {
                let name = p.file_name().unwrap().to_str().unwrap();
                name.starts_with(&prefix) && name.ends_with(".log")
            })
            .unwrap_or_else(|| panic!("expected a {prefix}*.log file, got {paths:?}"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("(000.000000) {iface} {frame}\n")
        );
    }
}
