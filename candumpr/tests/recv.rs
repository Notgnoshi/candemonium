use std::os::unix::io::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use candumpr::can::{self, CanFrame};
use candumpr::recv::dedicated::DedicatedRecv;
use candumpr::recv::epoll::EpollRecv;
use vcan_fixture::VcanHarness;

#[ctor::ctor]
fn setup() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(true)
        .init();
    vcan_fixture::enter_namespace();
}

const IFACE_COUNT: usize = 4;
const FRAMES_PER_IFACE: usize = 100;
const TOTAL_FRAMES: u64 = (IFACE_COUNT * FRAMES_PER_IFACE) as u64;

/// Open RX sockets first (so they receive frames), then send test frames on separate TX
/// sockets. Returns the RX sockets for the backend under test.
fn setup_rx_and_send(names: &[String], blocking: bool) -> Vec<OwnedFd> {
    let mut rx = Vec::with_capacity(names.len());
    for name in names {
        let sock = if blocking {
            can::open_can_raw_blocking(name).unwrap()
        } else {
            can::open_can_raw(name).unwrap()
        };
        rx.push(sock);
    }

    for (iface_idx, name) in names.iter().enumerate() {
        let tx = can::open_can_raw_blocking(name).unwrap();
        for frame_idx in 0..FRAMES_PER_IFACE {
            let frame = CanFrame::new(
                ((iface_idx as u32) << 8) | (frame_idx as u32) | libc::CAN_EFF_FLAG,
                &[iface_idx as u8, frame_idx as u8],
            );
            can::send_frame(tx.as_fd(), &frame).unwrap();
        }
    }

    rx
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn dedicated() {
    let vcans = VcanHarness::new(IFACE_COUNT).unwrap();
    let rx = setup_rx_and_send(vcans.names(), true);
    let backend = DedicatedRecv::new(rx);

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let count = AtomicU64::new(0);

    let total = backend
        .run(stop, &|_idx, _frame| {
            if count.fetch_add(1, Ordering::Relaxed) + 1 >= TOTAL_FRAMES {
                stop2.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();

    assert_eq!(total, TOTAL_FRAMES);
}

#[test]
#[cfg_attr(feature = "ci", ignore = "requires vcan")]
fn epoll() {
    let vcans = VcanHarness::new(IFACE_COUNT).unwrap();
    let rx = setup_rx_and_send(vcans.names(), false);
    let mut backend = EpollRecv::new(rx).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let mut count = 0u64;

    let total = backend
        .run(stop, &mut |_idx, _frame| {
            count += 1;
            if count >= TOTAL_FRAMES {
                stop2.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();

    assert_eq!(total, TOTAL_FRAMES);
}
