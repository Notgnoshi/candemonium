//! Per-frame instruction cost benchmark using gungraun (Callgrind).
//!
//! Setup: create vcan interfaces and pre-send frames into RX socket buffers.
//! Benchmark: drain all frames using each backend.
//!
//! The send cost is identical across all backends and cancels out in relative
//! comparisons. We pre-fill rather than send concurrently because vcan delivers
//! frames synchronously and silently drops them when the receiver buffer is full
//! (no backpressure to the sender).
//!
//! All callbacks use `AtomicU64::fetch_add` so the per-frame callback cost is
//! identical across backends (including the multi-threaded dedicated backend).
//!
//! Metrics: instructions, L1/L2 cache misses, branch mispredictions.
//!
//! Requires: Valgrind, vcan kernel module, gungraun-runner.

use std::os::unix::io::{AsFd, AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use candumpr::can::{self, CanFrame};
use candumpr::recv::dedicated::DedicatedRecv;
use candumpr::recv::epoll::EpollRecv;
use candumpr::recv::recvmmsg::RecvmmsgRecv;
use candumpr::recv::uring::UringRecv;
use candumpr::recv::uring_multi::UringMultiRecv;
use gungraun::{library_benchmark, library_benchmark_group, main};
use vcan_fixture::VcanHarness;

const IFACE_COUNT: usize = 4;

#[ctor::ctor]
fn init() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    vcan_fixture::enter_namespace();
}

struct Fixture {
    _vcans: VcanHarness,
    rx: Vec<OwnedFd>,
}

fn setup_blocking() -> Fixture {
    tracing::info!("setup_blocking: creating vcans");
    let vcans = VcanHarness::new(IFACE_COUNT).unwrap();
    tracing::info!("setup_blocking: prefilling");
    let rx = open_and_prefill(vcans.names(), true);
    tracing::info!("setup_blocking: done");
    Fixture { _vcans: vcans, rx }
}

fn setup_nonblocking() -> Fixture {
    tracing::info!("setup_nonblocking: creating vcans");
    let vcans = VcanHarness::new(IFACE_COUNT).unwrap();
    tracing::info!("setup_nonblocking: prefilling");
    let rx = open_and_prefill(vcans.names(), false);
    tracing::info!("setup_nonblocking: done");
    Fixture { _vcans: vcans, rx }
}

fn open_and_prefill(names: &[String], blocking: bool) -> Vec<OwnedFd> {
    let mut rx = Vec::with_capacity(names.len());
    for name in names {
        let sock = if blocking {
            can::open_can_raw_blocking(name).unwrap()
        } else {
            can::open_can_raw(name).unwrap()
        };
        rx.push(sock);
    }

    // Measure the actual per-frame kernel overhead (sk_buff truesize) by sending
    // one frame and observing the change in rmem_alloc via SO_MEMINFO. This
    // replaces the previous hardcoded SK_BUFF_OVERHEAD constant.
    let probe_tx = can::open_can_raw_blocking(&names[0]).unwrap();
    let before = get_rmem_alloc(&rx[0]);
    let probe = CanFrame::new(libc::CAN_EFF_FLAG, &[0; 8]);
    can::send_frame(probe_tx.as_fd(), &probe).unwrap();
    let after = get_rmem_alloc(&rx[0]);
    let truesize = (after - before) as usize;
    assert!(
        truesize > 0,
        "SO_MEMINFO rmem_alloc did not increase after send"
    );

    let rcvbuf = get_rcvbuf(&rx[0]);
    // Subtract 1 to account for the probe frame we already sent.
    let frames_per_iface = rcvbuf / truesize - 1;
    tracing::info!(
        truesize,
        rcvbuf,
        frames_per_iface,
        "measured buffer capacity"
    );

    for (iface_idx, name) in names.iter().enumerate() {
        let tx = can::open_can_raw_blocking(name).unwrap();
        // The first interface already has the probe frame; send one fewer.
        let count = if iface_idx == 0 {
            frames_per_iface
        } else {
            frames_per_iface + 1
        };
        for frame_idx in 0..count {
            let frame = CanFrame::new(
                ((iface_idx as u32) << 8) | (frame_idx as u32) | libc::CAN_EFF_FLAG,
                &[
                    iface_idx as u8,
                    frame_idx as u8,
                    0xFF,
                    0xFE,
                    0xFD,
                    0xFC,
                    0xFB,
                    0xFA,
                ],
            );
            can::send_frame(tx.as_fd(), &frame).unwrap();
        }
        tracing::debug!(iface = iface_idx, count, "iface prefilled");
    }

    rx
}

/// Read the current `rmem_alloc` for a socket via `SO_MEMINFO`.
fn get_rmem_alloc(fd: &OwnedFd) -> u32 {
    let mut info = [0u32; (libc::SK_MEMINFO_DROPS + 1) as usize];
    let mut len: libc::socklen_t = std::mem::size_of_val(&info) as u32;
    let ret = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_MEMINFO,
            info.as_mut_ptr().cast(),
            &mut len,
        )
    };
    assert_eq!(
        ret,
        0,
        "getsockopt(SO_MEMINFO): {}",
        std::io::Error::last_os_error()
    );
    info[libc::SK_MEMINFO_RMEM_ALLOC as usize]
}

/// Read the kernel receive buffer size (`SO_RCVBUF`).
fn get_rcvbuf(fd: &OwnedFd) -> usize {
    let mut val: libc::c_int = 0;
    let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as u32;
    let ret = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            std::ptr::from_mut(&mut val).cast(),
            &mut len,
        )
    };
    assert_eq!(
        ret,
        0,
        "getsockopt(SO_RCVBUF): {}",
        std::io::Error::last_os_error()
    );
    val as usize
}

/// Safety-net deadline so benchmarks terminate even if frames were dropped.
/// The actual drain completes in milliseconds; this just prevents hangs.
const DEADLINE: Duration = Duration::from_secs(1);

/// Spawn a thread that sets `stop` after `DEADLINE`. Returns the stop flag.
fn make_stop() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DEADLINE);
        stop2.store(true, Ordering::Relaxed);
    });
    stop
}

#[library_benchmark]
#[bench::run(setup = setup_blocking)]
fn dedicated(fixture: Fixture) -> u64 {
    let backend = DedicatedRecv::new(fixture.rx);
    let stop = make_stop();
    let count = AtomicU64::new(0);
    backend
        .run(stop, &|_idx, _frame| {
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap()
}

#[library_benchmark]
#[bench::run(setup = setup_nonblocking)]
fn epoll(fixture: Fixture) -> u64 {
    let mut backend = EpollRecv::new(fixture.rx).unwrap();
    let stop = make_stop();
    let count = AtomicU64::new(0);
    backend
        .run(stop, &mut |_idx, _frame| {
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap()
}

#[library_benchmark]
#[bench::run(setup = setup_nonblocking)]
fn recvmmsg(fixture: Fixture) -> u64 {
    let mut backend = RecvmmsgRecv::new(fixture.rx).unwrap();
    let stop = make_stop();
    let count = AtomicU64::new(0);
    backend
        .run(stop, &mut |_idx, _frame| {
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap()
}

#[library_benchmark]
#[bench::run(setup = setup_nonblocking)]
fn uring(fixture: Fixture) -> u64 {
    let mut backend = UringRecv::new(fixture.rx).unwrap();
    let stop = make_stop();
    let count = AtomicU64::new(0);
    backend
        .run(stop, &mut |_idx, _frame| {
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap()
}

#[library_benchmark]
#[bench::run(setup = setup_nonblocking)]
fn uring_multi(fixture: Fixture) -> u64 {
    let mut backend = UringMultiRecv::new(fixture.rx).unwrap();
    let stop = make_stop();
    let count = AtomicU64::new(0);
    backend
        .run(stop, &mut |_idx, _frame| {
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap()
}

library_benchmark_group!(
    name = recv_cost;
    compare_by_id = true;
    benchmarks = dedicated, epoll, recvmmsg, uring, uring_multi
);

main!(library_benchmark_groups = recv_cost);
