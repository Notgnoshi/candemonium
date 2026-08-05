use std::os::unix::io::AsFd;
use std::path::Path;
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

/// The `i<digits>_` filename indexes present in `dir`, sorted.
fn indexes(dir: &Path) -> Vec<u64> {
    let mut indexes: Vec<u64> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.strip_prefix('i')?.split_once('_')?.0.parse().ok()
        })
        .collect();
    indexes.sort_unstable();
    indexes
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn retention_bounds_the_interface_directory() {
    let vcans = VcanHarness::new(1).unwrap();
    let iface = &vcans.names()[0];
    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().join("log");
    let iface_dir = log_dir.join(iface.as_str());

    // A directory left over from an earlier "run": two stale logs and a foreign file. The 70KB
    // total is over the 50KB limit, but deleting the foreign file alone gets back under it.
    std::fs::create_dir_all(&iface_dir).unwrap();
    let stale = |index: u64| iface_dir.join(format!("i{index:04}_{iface}_stale.log"));
    std::fs::write(stale(0), vec![b'x'; 20_000]).unwrap();
    std::fs::write(stale(1), vec![b'x'; 20_000]).unwrap();
    std::fs::write(iface_dir.join("junk.txt"), vec![b'x'; 30_000]).unwrap();

    let cfg = tmp.path().join("candumpr.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"
            [interface.{iface}]
            directory = "{}"
            compress = false
            flush_every = "1KB"
            rotate_every = "10KB"
            retain = "50KB"
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

    // The first frame activates the sink, which is when startup enforcement fires.
    let tx = can::open_can_raw_blocking(iface).unwrap();
    can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &[0])).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while iface_dir.join("junk.txt").exists() {
        assert!(
            Instant::now() < deadline,
            "startup enforcement never deleted junk.txt"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The foreign file went first: it alone covered the excess, so the stale logs survive.
    assert_eq!(indexes(&iface_dir), [0, 1, 2]);

    // Pump traffic through several rotations, until the limit has re-tripped enough times to
    // delete both stale logs.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut sent = 0u32;
    while indexes(&iface_dir)[0] <= 2 {
        assert!(
            Instant::now() < deadline,
            "stale logs survived {sent} frames"
        );
        for _ in 0..50 {
            can::send_frame(tx.as_fd(), &LinuxCanFrame::new(0x123, &sent.to_be_bytes())).unwrap();
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    child.signal(libc::SIGTERM).unwrap();
    let output = child.captured_output().unwrap();
    assert!(
        output.status.success(),
        "expected a clean exit, got {}",
        output.status
    );

    // The directory ends within the retention limit
    let total: u64 = std::fs::read_dir(&iface_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert!(total <= 50_000, "final total over the limit: {total} bytes");
}
