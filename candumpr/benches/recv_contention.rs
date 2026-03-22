//! Benchmark C: backend behavior under CPU contention.
//!
//! Runs each backend with 4 interfaces at 4000 fps while CPU burner threads consume
//! 75% or 90% of available cycles. Measures whether backends degrade gracefully or
//! drop frames when starved of CPU.
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

    for &load_pct in &[75, 90] {
        eprintln!("\n--- CPU load: {load_pct}% ---\n");
        let mut results = Vec::new();
        for backend in common::BACKENDS {
            let (burn_stop, burn_handles) = vcan_fixture::bench::start_cpu_load(4, load_pct);
            let mean = common::run_repetitions(backend, 4, 4000, RUN_DURATION, REPETITIONS);
            vcan_fixture::bench::stop_cpu_load(burn_stop, burn_handles);
            results.push(mean);
        }
        common::print_results(&results);
    }
}
