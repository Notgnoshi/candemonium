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

/// Wait for one log file to appear under `dir/<iface>/` for every interface, and return them in the
/// same order as `ifaces`.
#[track_caller]
fn wait_for_logs(dir: &Path, ifaces: &[&str]) -> Vec<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found: Vec<PathBuf> = ifaces
            .iter()
            .filter_map(|iface| {
                std::fs::read_dir(dir.join(iface))
                    .ok()?
                    .flatten()
                    .map(|e| e.path())
                    .next()
            })
            .collect();
        if found.len() == ifaces.len() {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "candumpr created {} of {} log files under {dir:?}",
            found.len(),
            ifaces.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn logs_each_interface_to_its_own_subdirectory() {
    let vcans = VcanHarness::new(2).unwrap();
    let iface1 = &vcans.names()[0];
    let iface2 = &vcans.names()[1];
    // Two directories, so a stray config file can never be mistaken for a log file.
    let cfg_dir = tempfile::TempDir::new().unwrap();
    let log_dir = tempfile::TempDir::new().unwrap();
    let cfg = cfg_dir.path().join("candumpr.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"
            [defaults]
            directory = "{}"
            compress = false
            timestamp = "zero"

            [interface.{iface1}]
            compress = true

            [interface.{iface2}]
            "#,
            log_dir.path().display()
        ),
    )
    .unwrap();

    let child = tool!("candumpr")
        .arg(format!("--daemon={}", cfg.display()))
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its sockets and start receiving
    std::thread::sleep(Duration::from_millis(200));

    let tx1 = can::open_can_raw_blocking(iface1).unwrap();
    let tx2 = can::open_can_raw_blocking(iface2).unwrap();
    can::send_frame(tx1.as_fd(), &LinuxCanFrame::new(0x123, &[0xAB])).unwrap();
    can::send_frame(tx2.as_fd(), &LinuxCanFrame::new(0x456, &[0xCD])).unwrap();

    let paths = wait_for_logs(log_dir.path(), &[iface1.as_str(), iface2.as_str()]); // panics on timeout

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    let name = paths[0].file_name().unwrap().to_str().unwrap();
    let prefix = format!("i0000_{iface1}_");
    assert_eq!(paths[0].parent().unwrap(), log_dir.path().join(iface1));
    assert!(
        name.starts_with(&prefix) && name.ends_with(".log.zst"),
        "expected a {prefix}*.log.zst file, got {name}"
    );

    let name = paths[1].file_name().unwrap().to_str().unwrap();
    let prefix = format!("i0000_{iface2}_");
    assert_eq!(paths[1].parent().unwrap(), log_dir.path().join(iface2));
    assert!(
        name.starts_with(&prefix) && name.ends_with(".log"),
        "expected a {prefix}*.log file, got {name}"
    );

    assert_eq!(
        std::fs::read_to_string(&paths[1]).unwrap(),
        format!("(000.000000) {iface2} 456#CD\n")
    );
}

#[test]
fn an_invalid_config_exits_nonzero() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = dir.path().join("candumpr.toml");
    // Two faults at once: an unimplemented key to warn about, and no `directory` anywhere.
    std::fs::write(
        &cfg,
        r#"
        [defaults]
        compress = false

        [defaults.rotation]
        limit = "100MB"

        [interface.can0]
        "#,
    )
    .unwrap();

    let output = tool!("candumpr")
        .arg(format!("--daemon={}", cfg.display()))
        .captured_output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected a nonzero exit, got {}",
        output.status
    );
    // Use separate .contains() calls to skip over the tracing fmt ANSI sequences
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown configuration key") && stderr.contains("defaults.rotation"),
        "expected the unimplemented key to be warned about, got:\n{stderr}"
    );
    assert!(
        stderr.contains("invalid config file") && stderr.contains("missing `directory` setting"),
        "expected the config file path alongside the validation failure, got:\n{stderr}"
    );
}
