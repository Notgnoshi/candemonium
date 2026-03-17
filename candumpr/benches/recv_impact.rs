//! Benchmark B: system impact under sustained load.
//!
//! Measures per-backend CPU time (user+sys), context switches, and frame loss across multiple
//! interface counts and send rates.
//!
//! Uses RUSAGE_THREAD to isolate receiver cost from sender overhead.
//!
//! Requires: vcan kernel module.

mod common;

use std::time::Duration;

const RUN_DURATION: Duration = Duration::from_secs(5);
const REPETITIONS: usize = 4;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    vcan_fixture::enter_namespace();
    vcan_fixture::bench::pin_to_cores(4);

    let mut results = Vec::new();
    for backend in common::BACKENDS {
        for &ifaces in &[1, 2, 4] {
            for &rate in &[1000, 2000, 4000] {
                let mean =
                    common::run_repetitions(backend, ifaces, rate, RUN_DURATION, REPETITIONS);
                results.push(mean);
            }
        }
    }
    common::print_results(&results);
}
