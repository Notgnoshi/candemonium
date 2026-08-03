use std::os::unix::io::AsFd;
use std::path::{Path, PathBuf};
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

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
}

/// Send frames until `dir` holds `want` log files.
#[track_caller]
fn send_until_n_files_exist(iface: &str, dir: &Path, want: usize) {
    let tx = can::open_can_raw_blocking(iface).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while log_files(dir).len() < want {
        can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();
        assert!(
            Instant::now() < deadline,
            "only {} of {want} log files in {dir:?}",
            log_files(dir).len()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Rotation must write the zstd epilogue, not merely close the file.
///
/// A log that was never finished decompresses completely but exits nonzero because zstd looks for
/// the epilogue.
#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn sighup_rotated_files_are_independently_decompressable() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let dir = tempfile::TempDir::new().unwrap();

    let child = tool!("candumpr")
        .args(["-l", "--compress"])
        .arg(iface)
        .current_dir(dir.path())
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    send_until_n_files_exist(iface, dir.path(), 1);
    child.signal(libc::SIGHUP).unwrap();
    send_until_n_files_exist(iface, dir.path(), 2);

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    let paths = log_files(dir.path());
    assert_eq!(paths.len(), 2, "expected two files; got: {paths:?}");
    for path in &paths {
        let out = std::process::Command::new("zstd")
            .arg("-dc")
            .arg(path)
            .output()
            .unwrap();
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(
            out.status.success(),
            "zstd -d exited {} on {path:?}",
            out.status
        );
        // Without this an empty-but-finished frame would still satisfy the status check
        assert!(!out.stdout.is_empty(), "{path:?} decoded to nothing");
    }
}
