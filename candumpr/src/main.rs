use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use candumpr::can;
use candumpr::format::{CanutilsFormatter, Formatter};
use candumpr::recv::receiver::{BATCH_CAPACITY, Receiver};
use candumpr::writer::{StdoutWriter, Writer};
use clap::Parser;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// Log CAN traffic from multiple networks.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// CAN interfaces to listen on.
    #[arg(required = true)]
    interfaces: Vec<String>,

    /// Log level for tracing output on stderr.
    #[arg(long, default_value = "INFO")]
    log_level: tracing::Level,
}

fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(cli.log_level)
        .init();

    let sockets: Vec<_> = cli
        .interfaces
        .iter()
        .map(|name| can::open_can_raw(name))
        .collect::<std::io::Result<_>>()?;

    const POOL_SIZE: usize = 4;
    const RECYCLE_BOUND: usize = 8;

    let (full_tx, full_rx) = mpsc::channel::<Vec<_>>();
    let (empty_tx, empty_rx) = mpsc::sync_channel::<Vec<_>>(RECYCLE_BOUND);
    for _ in 0..POOL_SIZE {
        empty_tx
            .send(Vec::with_capacity(BATCH_CAPACITY))
            .expect("recycle channel must accept initial pool");
    }

    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
    }

    let recv_handle = std::thread::spawn(move || -> eyre::Result<u64> {
        let mut recv = Receiver::new(sockets)?;
        let total = recv.run(&STOP, &full_tx, &empty_rx)?;
        Ok(total)
    });

    let formatter = CanutilsFormatter::new(cli.interfaces);
    let mut writer = StdoutWriter::new();
    let mut buf = Vec::with_capacity(4096);

    while !STOP.load(Ordering::Relaxed) {
        match full_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(mut batch) => {
                for frame in &batch {
                    formatter.format(frame, &mut buf);
                }
                writer.write(&buf)?;
                buf.clear();
                batch.clear();
                let _ = empty_tx.try_send(batch);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Drain remaining batches after stop.
    while let Ok(mut batch) = full_rx.try_recv() {
        for frame in &batch {
            formatter.format(frame, &mut buf);
        }
        batch.clear();
        let _ = empty_tx.try_send(batch);
    }
    if !buf.is_empty() {
        writer.write(&buf)?;
    }

    let total = recv_handle.join().expect("receiver thread panicked")?;
    tracing::debug!(total, "receiver finished");

    Ok(())
}
