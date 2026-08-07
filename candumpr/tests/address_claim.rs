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

const REQUEST: &str = "18EAFFFE#00EE00";

/// The path of the `i<index>_` log file in `dir`, if it exists yet.
fn indexed_file(dir: &Path, index: u64) -> Option<PathBuf> {
    let prefix = format!("i{index:04}_");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
}

/// Contents of the `i<index>_` log file in `dir`, or empty if it doesn't exist yet.
fn contents(dir: &Path, index: u64) -> String {
    indexed_file(dir, index)
        .map(|path| std::fs::read_to_string(path).unwrap())
        .unwrap_or_default()
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn each_opened_file_gets_a_request() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().join("log");
    let iface_dir = log_dir.join(iface.as_str());

    // flush_every = "1B" puts every write on disk immediately, so polling on file contents is
    // deterministic. rotate_every = "off" means only the SIGHUP below rotates.
    let cfg = tmp.path().join("candumpr.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"
            [interface.{iface}]
            directory = "{}"
            compress = false
            flush_every = "1B"
            rotate_every = "off"
            retain = "off"
            "#,
            log_dir.display()
        ),
    )
    .unwrap();

    let child = tool!("candumpr")
        .arg(format!("--daemon={}", cfg.display()))
        .spawn_piped()
        .unwrap();

    // Give enough time for candumpr to create its socket and start receiving
    std::thread::sleep(Duration::from_millis(200));

    // The first frame activates the sink, which requests the address claims.
    let tx = can::open_can_raw_blocking(iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0])).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = contents(&iface_dir, 0);
        if text.contains("123#00") && text.contains(REQUEST) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "i0000 never got the frame and the request: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    child.signal(libc::SIGHUP).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while indexed_file(&iface_dir, 1).is_none() {
        assert!(Instant::now() < deadline, "i0001 never appeared");
        can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x456, &[1])).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while !contents(&iface_dir, 1).contains(REQUEST) {
        assert!(
            Instant::now() < deadline,
            "i0001 never got the request: {:?}",
            contents(&iface_dir, 1)
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );
}
