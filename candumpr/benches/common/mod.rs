use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use candumpr::can::{self, CanFrame};
use candumpr::recv::dedicated::DedicatedRecv;
use candumpr::recv::epoll::EpollRecv;
use candumpr::recv::recvmmsg::RecvmmsgRecv;
use candumpr::recv::uring::UringRecv;
use candumpr::recv::uring_multi::UringMultiRecv;
use tabled::Tabled;
use vcan_fixture::VcanHarness;
use vcan_fixture::bench::{Rusage, getrusage_thread};

// --- Backend dispatch ---

type BackendRunFn = fn(Vec<OwnedFd>, Arc<AtomicBool>, &AtomicU64) -> (u64, Rusage);

pub struct BackendDef {
    pub name: &'static str,
    pub blocking: bool,
    pub run: BackendRunFn,
}

pub const BACKENDS: &[BackendDef] = &[
    BackendDef {
        name: "dedicated",
        blocking: true,
        run: run_dedicated,
    },
    BackendDef {
        name: "epoll",
        blocking: false,
        run: run_epoll,
    },
    BackendDef {
        name: "recvmmsg",
        blocking: false,
        run: run_recvmmsg,
    },
    BackendDef {
        name: "uring",
        blocking: false,
        run: run_uring,
    },
    BackendDef {
        name: "uring_multi",
        blocking: false,
        run: run_uring_multi,
    },
];

// --- Sequence checker ---

fn frame_seq(frame: &CanFrame) -> u32 {
    u32::from_le_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]])
}

struct SeqCheck {
    expected: Vec<AtomicU32>,
}

impl SeqCheck {
    fn new(n: usize) -> Self {
        Self {
            expected: (0..n).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    fn check(&self, idx: usize, frame: &CanFrame) {
        let actual = frame_seq(frame);
        let expected = self.expected[idx].load(Ordering::Relaxed);
        if actual != expected {
            tracing::warn!(
                iface = idx,
                received = actual,
                expected = expected,
                "out-of-sequence frame"
            );
        }
        self.expected[idx].store(actual.wrapping_add(1), Ordering::Relaxed);
    }
}

// --- Backend run functions ---
//
// Single-threaded backends: wrap the backend's run() with getrusage_thread() before/after.
// Dedicated backend: uses run_instrumented() to collect per-thread RUSAGE_THREAD and
// aggregate the deltas.

fn run_dedicated(sockets: Vec<OwnedFd>, stop: Arc<AtomicBool>, count: &AtomicU64) -> (u64, Rusage) {
    let seq = SeqCheck::new(sockets.len());
    let backend = DedicatedRecv::new(sockets);
    let rusage = std::sync::Mutex::new(Rusage::default());
    let total = backend
        .run_instrumented(
            stop,
            &|idx, frame, _meta| {
                seq.check(idx, frame);
                count.fetch_add(1, Ordering::Relaxed);
            },
            &|_idx, inner| {
                let before = getrusage_thread();
                let count = inner()?;
                *rusage.lock().unwrap() += getrusage_thread().delta(&before);
                Ok(count)
            },
        )
        .unwrap();
    (total, rusage.into_inner().unwrap())
}

fn run_epoll(sockets: Vec<OwnedFd>, stop: Arc<AtomicBool>, count: &AtomicU64) -> (u64, Rusage) {
    let seq = SeqCheck::new(sockets.len());
    let mut backend = EpollRecv::new(sockets).unwrap();
    let before = getrusage_thread();
    let total = backend
        .run(stop, &mut |idx, frame, _meta| {
            seq.check(idx, frame);
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
    let after = getrusage_thread();
    (total, after.delta(&before))
}

fn run_recvmmsg(sockets: Vec<OwnedFd>, stop: Arc<AtomicBool>, count: &AtomicU64) -> (u64, Rusage) {
    let seq = SeqCheck::new(sockets.len());
    let mut backend = RecvmmsgRecv::new(sockets).unwrap();
    let before = getrusage_thread();
    let total = backend
        .run(stop, &mut |idx, frame, _meta| {
            seq.check(idx, frame);
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
    let after = getrusage_thread();
    (total, after.delta(&before))
}

fn run_uring(sockets: Vec<OwnedFd>, stop: Arc<AtomicBool>, count: &AtomicU64) -> (u64, Rusage) {
    let seq = SeqCheck::new(sockets.len());
    let mut backend = UringRecv::new(sockets).unwrap();
    let before = getrusage_thread();
    let total = backend
        .run(stop, &mut |idx, frame, _meta| {
            seq.check(idx, frame);
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
    let after = getrusage_thread();
    (total, after.delta(&before))
}

fn run_uring_multi(
    sockets: Vec<OwnedFd>,
    stop: Arc<AtomicBool>,
    count: &AtomicU64,
) -> (u64, Rusage) {
    let seq = SeqCheck::new(sockets.len());
    let mut backend = UringMultiRecv::new(sockets).unwrap();
    let before = getrusage_thread();
    let total = backend
        .run(stop, &mut |idx, frame, _meta| {
            seq.check(idx, frame);
            count.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
    let after = getrusage_thread();
    (total, after.delta(&before))
}

// --- Sender ---

/// Send frames at a fixed rate for `duration`, then set `stop` to signal the receiver.
fn sender_loop(
    tx: BorrowedFd<'_>,
    iface_idx: usize,
    interval: Duration,
    duration: Duration,
    stop: &AtomicBool,
    count: &AtomicU64,
) {
    let mut frame_idx = 0u32;
    let start = Instant::now();
    let mut next = start;
    while start.elapsed() < duration {
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        let frame = make_frame(iface_idx, frame_idx);
        if can::send_frame(tx, &frame).is_ok() {
            count.fetch_add(1, Ordering::Relaxed);
        }
        frame_idx += 1;
    }
    stop.store(true, Ordering::Relaxed);
}

fn make_frame(iface_idx: usize, frame_idx: u32) -> CanFrame {
    let seq = frame_idx.to_le_bytes();
    CanFrame::new(
        ((iface_idx as u32) << 8) | (frame_idx & 0xFF) | libc::CAN_EFF_FLAG,
        &[
            seq[0],
            seq[1],
            seq[2],
            seq[3],
            iface_idx as u8,
            0xEF,
            0xCA,
            0xFE,
        ],
    )
}

// --- Result ---

struct RawRun {
    sent: u64,
    recv: u64,
    user_us: i64,
    sys_us: i64,
    vol_csw: i64,
    invol_csw: i64,
}

#[derive(Tabled)]
pub struct RunResult {
    pub backend: &'static str,
    pub ifaces: usize,
    pub rate: u32,
    pub sent: u64,
    pub recv: u64,
    pub lost: u64,
    pub user_ms: String,
    pub sys_ms: String,
    pub vol_csw: i64,
    pub invol_csw: i64,
}

pub fn print_results(results: &[RunResult]) {
    use tabled::settings::Style;
    let table = tabled::Table::new(results)
        .with(Style::markdown())
        .to_string();
    eprintln!("{table}");
}

// --- Run config ---

fn run_config(backend: &BackendDef, ifaces: usize, rate: u32, duration: Duration) -> RawRun {
    tracing::info!(
        backend = backend.name,
        ifaces,
        rate,
        ?duration,
        "starting run"
    );
    let vcans = VcanHarness::new(ifaces).unwrap();

    let mut tx_sockets = Vec::with_capacity(ifaces);
    let mut rx_sockets = Vec::with_capacity(ifaces);
    for name in vcans.names() {
        let rx = if backend.blocking {
            can::open_can_raw_blocking(name).unwrap()
        } else {
            can::open_can_raw(name).unwrap()
        };
        rx_sockets.push(rx);
        tx_sockets.push(can::open_can_raw_blocking(name).unwrap());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let send_count = AtomicU64::new(0);
    let recv_count = AtomicU64::new(0);
    let interval = Duration::from_secs_f64(1.0 / rate as f64);

    let (recv, rusage) = std::thread::scope(|scope| {
        // Senders: one per interface, paced by sleeping between frames.
        // Each sender runs for `duration` then sets the stop flag.
        let stop_ref: &AtomicBool = &stop;
        let send_count_ref = &send_count;
        for (idx, tx) in tx_sockets.iter().enumerate() {
            scope.spawn(move || {
                tracing::debug!(iface = idx, "sender started");
                sender_loop(
                    tx.as_fd(),
                    idx,
                    interval,
                    duration,
                    stop_ref,
                    send_count_ref,
                );
                tracing::debug!(iface = idx, "sender done");
            });
        }

        // Receiver: backend under test on the calling thread.
        tracing::debug!("receiver started");
        let result = (backend.run)(rx_sockets, stop.clone(), &recv_count);
        tracing::debug!("receiver done");
        result
    });

    let sent = send_count.load(Ordering::Relaxed);
    tracing::info!(sent, recv, "run complete");

    RawRun {
        sent,
        recv,
        user_us: rusage.user_us,
        sys_us: rusage.sys_us,
        vol_csw: rusage.vol_csw,
        invol_csw: rusage.invol_csw,
    }
}

/// Run `reps` repetitions and return the averaged result.
pub fn run_repetitions(
    backend: &BackendDef,
    ifaces: usize,
    rate: u32,
    duration: Duration,
    reps: usize,
) -> RunResult {
    let runs: Vec<RawRun> = (0..reps)
        .map(|_| run_config(backend, ifaces, rate, duration))
        .collect();
    let n = reps as u64;
    let ni = reps as i64;
    let sent = runs.iter().map(|r| r.sent).sum::<u64>() / n;
    let recv = runs.iter().map(|r| r.recv).sum::<u64>() / n;
    let user_us = runs.iter().map(|r| r.user_us).sum::<i64>() / ni;
    let sys_us = runs.iter().map(|r| r.sys_us).sum::<i64>() / ni;
    RunResult {
        backend: backend.name,
        ifaces,
        rate,
        sent,
        recv,
        lost: sent.saturating_sub(recv),
        user_ms: format!("{:.1}", user_us as f64 / 1000.0),
        sys_ms: format!("{:.1}", sys_us as f64 / 1000.0),
        vol_csw: runs.iter().map(|r| r.vol_csw).sum::<i64>() / ni,
        invol_csw: runs.iter().map(|r| r.invol_csw).sum::<i64>() / ni,
    }
}
