//! Benchmark utilities: rusage wrappers, CPU pinning, CPU load generation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Per-thread or per-process resource usage snapshot.
#[derive(Clone, Default)]
pub struct Rusage {
    pub user_us: i64,
    pub sys_us: i64,
    pub vol_csw: i64,
    pub invol_csw: i64,
}

impl Rusage {
    /// Compute the delta from `before` to `self`.
    pub fn delta(&self, before: &Rusage) -> Rusage {
        Rusage {
            user_us: self.user_us - before.user_us,
            sys_us: self.sys_us - before.sys_us,
            vol_csw: self.vol_csw - before.vol_csw,
            invol_csw: self.invol_csw - before.invol_csw,
        }
    }
}

impl std::ops::AddAssign for Rusage {
    fn add_assign(&mut self, rhs: Rusage) {
        self.user_us += rhs.user_us;
        self.sys_us += rhs.sys_us;
        self.vol_csw += rhs.vol_csw;
        self.invol_csw += rhs.invol_csw;
    }
}

fn getrusage_raw(who: i32) -> Rusage {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(who, &mut ru) };
    Rusage {
        user_us: ru.ru_utime.tv_sec * 1_000_000 + ru.ru_utime.tv_usec,
        sys_us: ru.ru_stime.tv_sec * 1_000_000 + ru.ru_stime.tv_usec,
        vol_csw: ru.ru_nvcsw,
        invol_csw: ru.ru_nivcsw,
    }
}

/// Get resource usage for the calling thread (`RUSAGE_THREAD`).
pub fn getrusage_thread() -> Rusage {
    getrusage_raw(libc::RUSAGE_THREAD)
}

/// Get resource usage for the entire process (`RUSAGE_SELF`).
pub fn getrusage_self() -> Rusage {
    getrusage_raw(libc::RUSAGE_SELF)
}

/// Pin the current process to cores `0..n` using `sched_setaffinity`.
///
/// All threads (senders, receiver, CPU burners) inherit this affinity.
/// Falls back gracefully if the system has fewer cores than requested.
pub fn pin_to_cores(n: usize) {
    let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as usize;
    let cores = n.min(online);
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for i in 0..cores {
            libc::CPU_SET(i, &mut set);
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

/// Spawn `n` CPU burner threads at `load_pct`% duty cycle.
///
/// Each thread busy-spins for `load_pct`% of a 10ms period, then sleeps the rest.
/// Threads inherit the process CPU affinity, so they compete on the same cores
/// as the benchmark.
pub fn start_cpu_load(n: usize, load_pct: u32) -> (Arc<AtomicBool>, Vec<JoinHandle<()>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(n);
    let period = Duration::from_millis(10);
    let busy = period * load_pct / 100;

    for _ in 0..n {
        let stop = stop.clone();
        handles.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();

                // Busy-spin phase.
                while start.elapsed() < busy {
                    std::hint::spin_loop();
                }

                // Sleep phase.
                let remaining = period.saturating_sub(start.elapsed());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining);
                }
            }
        }));
    }

    (stop, handles)
}

/// Stop all CPU burner threads and wait for them to finish.
pub fn stop_cpu_load(stop: Arc<AtomicBool>, handles: Vec<JoinHandle<()>>) {
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
}
