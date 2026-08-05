use std::os::unix::io::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
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

/// Decompress with the zstd CLI, which is the whole point of using a standard format.
///
/// Returns the status as well as the output so that we can handle the case where a log that was
/// never closed decodes completely but exits nonzero (all windows written, but no frame epilogue).
fn zstd_d(path: &Path) -> (ExitStatus, Vec<u8>) {
    let out = std::process::Command::new("zstd")
        .arg("-dc")
        .arg(path)
        .output()
        .unwrap();
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    (out.status, out.stdout)
}

/// The single log file candumpr is writing in `dir`, once it has created one.
///
/// `dir` itself may not exist yet: daemon mode creates the per-interface subdirectory together
/// with the first log file.
fn log_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .next()
        .map(|entry| entry.unwrap().path())
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn logs_compressed_to_a_zst_file() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let dir = tempfile::TempDir::new().unwrap();

    let child = tool!("candumpr")
        .args(["-l", "--compress", "--timestamp", "zero"])
        .arg(iface)
        .current_dir(dir.path())
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    let tx = can::open_can_raw_blocking(iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();

    // The file is created by the same write that takes the frame, and the clean shutdown below
    // writes everything out, so its mere existence is enough to signal on.
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = loop {
        if let Some(path) = log_file(dir.path()) {
            break path;
        }
        assert!(Instant::now() < deadline, "candumpr created no log file");
        std::thread::sleep(Duration::from_millis(20));
    };
    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    let name = path.file_name().unwrap().to_str().unwrap();
    let prefix = format!("i0000_{iface}_");
    assert!(
        name.starts_with(&prefix) && name.ends_with(".log.zst"),
        "expected a {prefix}*.log.zst file, got {name}"
    );

    // A cleanly closed log has its zstd epilogue, so the CLI is happy.
    let (status, stdout) = zstd_d(&path);
    assert!(status.success(), "zstd -d exited {status}");
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        format!("(000.000000) {iface} 123#AB\n")
    );
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn sigkill_leaves_a_decodable_prefix() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config").join("candumpr.toml");
    std::fs::create_dir_all(tmp.path().join("config")).unwrap();
    // Use daemon mode so we can specify the flush interval in bytes rather than time, so that the
    // test runs faster.
    std::fs::write(
        &cfg,
        format!(
            r#"
            [defaults]
            directory = "{}"
            flush_every = "100 B"

            [interface.{iface}]
            "#,
            tmp.path().join("log").display()
        ),
    )
    .unwrap();

    let child = tool!("candumpr")
        .arg(format!("--daemon={}", cfg.display()))
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    // Keep traffic flowing and kill the moment the first bytes reach the file, so that the kill
    // lands in the middle of a zstd block rather than after one.
    let tx = can::open_can_raw_blocking(iface).unwrap();
    let mut sent = 0u32;
    let log_subdir = tmp.path().join("log").join(iface.as_str());
    let path = loop {
        for _ in 0..50 {
            can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &sent.to_be_bytes())).unwrap();
            sent += 1;
        }
        if let Some(path) = log_file(&log_subdir)
            && path.metadata().unwrap().len() > 0
        {
            break path;
        }
        assert!(
            sent < 50_000,
            "no bytes reached the log after {sent} frames"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    child.signal(libc::SIGKILL).unwrap();
    let _ = child.captured_output();

    let (status, stdout) = zstd_d(&path);
    // No epilogue was ever written, so the CLI reports the stream ended early even though it
    // handed us everything that was in it.
    assert!(
        !status.success(),
        "a log that was never closed does not decode cleanly; got status: {status}"
    );

    let text = String::from_utf8(stdout).unwrap();
    assert!(!text.is_empty(), "recovered nothing from {path:?}");
    // ZstdWriter only writes bytes to the FileWriter on ZstdWriter::flush(), which only ever
    // happens between frames. This is to ensure that partial frames are never written outside of
    // power loss.
    assert!(
        text.ends_with('\n'),
        "recovered a partial frame, ending {:?}",
        &text[text.len().saturating_sub(60)..]
    );
    for (i, line) in text.lines().enumerate() {
        let counter = line.rsplit('#').next().unwrap();
        assert_eq!(
            u32::from_str_radix(counter, 16).unwrap(),
            i as u32,
            "line {i} is out of sequence: {line}"
        );
    }
}
