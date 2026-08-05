use std::os::unix::io::AsFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use candumpr::can;
use candumpr::config::{Cli, Config, Format};
use candumpr::debounce::Debounce;
use candumpr::errframe::{BusState, ErrorFrame};
use candumpr::format::{CanutilsConsoleFormatter, CanutilsFileFormatter, Formatter};
use candumpr::frame::CanFrame;
use candumpr::pipeline::Pipeline;
use candumpr::recv::netlink::{self, LinkEvent};
use candumpr::recv::receiver::{BATCH_CAPACITY, Receiver};
use candumpr::sink::{Sink, SinkConfig};
use clap::Parser;
use crossbeam_channel::select;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);
static SIGNAL_ROTATE: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(sig: libc::c_int) {
    if sig == libc::SIGHUP {
        SIGNAL_ROTATE.store(true, Ordering::Relaxed);
    } else {
        SIGNAL_STOP.store(true, Ordering::Relaxed);
    }
}

/// True if any error in the chain is an EPIPE.
fn is_broken_pipe(err: &eyre::Report) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
}

/// Log a link-state edge to stderr, ignoring repeats of the last observed state.
///
/// Returns true if the interface transitioned to down.
fn handle_link_event(event: LinkEvent, link_up: &mut [Option<bool>], names: &[String]) -> bool {
    let (sock_id, up) = match event {
        LinkEvent::LinkUp { sock_id } => (sock_id, true),
        LinkEvent::LinkDown { sock_id } => (sock_id, false),
    };
    if link_up[sock_id] == Some(up) {
        return false;
    }
    link_up[sock_id] = Some(up);
    let interface = &names[sock_id];
    if up {
        tracing::info!(interface = %interface, "interface link up");
    } else {
        tracing::warn!(interface = %interface, "interface link down");
    }
    !up
}

/// Log each error frame in `batch` at debug level, and log bus-state transitions (edges only).
///
/// Returns true if any interface transitioned into bus-off.
fn log_error_frames(batch: &[CanFrame], bus_state: &mut [BusState], names: &[String]) -> bool {
    let mut bus_off = false;
    for frame in batch {
        let Some(err) = ErrorFrame::parse(&frame.raw) else {
            continue;
        };
        let interface = &names[frame.sock_id];
        tracing::debug!(interface = %interface, error = %err, "CAN error frame");

        let Some(new) = err.bus_state() else {
            continue;
        };
        let old = bus_state[frame.sock_id];
        if old == new {
            continue;
        }
        bus_state[frame.sock_id] = new;
        match new {
            BusState::ErrorActive => {
                tracing::info!(interface = %interface, "bus state {old} -> {new}")
            }
            BusState::ErrorWarning | BusState::ErrorPassive => {
                tracing::warn!(interface = %interface, "bus state {old} -> {new}")
            }
            BusState::BusOff => {
                tracing::error!(interface = %interface, "bus state {old} -> {new}");
                bus_off = true;
            }
        }
    }
    bus_off
}

fn main() -> ExitCode {
    if let Err(e) = color_eyre::install() {
        eprintln!("failed to install error handler: {e:#}");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();

    // neli SPAMS netlink event logs on startup.
    let filter = Targets::new()
        .with_default(cli.log_level)
        .with_target("neli", tracing::Level::WARN);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .finish()
        .with(filter)
        .init();

    let config = match Config::resolve(cli) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = ?e, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };

    // The sockets vector defines the canonical interface ordering. The orderings of:
    //
    // 1. config.interfaces
    // 2. sockets
    // 3. Pipeline::sinks / bufs
    // 4. et al.
    //
    // all follow the same ordering, and are all indexed by CanFrame::sock_id
    let names = config.interfaces.clone();
    let sockets: Vec<_> = match names
        .iter()
        .map(|name| can::open_can_raw(name))
        .collect::<std::io::Result<_>>()
    {
        Ok(sockets) => sockets,
        Err(e) => {
            tracing::error!(error = ?e, "failed to open CAN sockets");
            return ExitCode::FAILURE;
        }
    };

    for (name, sock) in names.iter().zip(&sockets) {
        match can::get_recv_buffer(sock.as_fd()) {
            Ok(bytes) => {
                tracing::info!(interface = %name, rcvbuf_bytes = bytes, "opened CAN socket")
            }
            Err(e) => tracing::warn!(interface = %name, error = ?e, "failed to query rcvbuf size"),
        }
    }

    const POOL_SIZE: usize = 4;
    const RECYCLE_BOUND: usize = 8;

    let (full_tx, full_rx) = crossbeam_channel::unbounded::<Vec<_>>();
    let (empty_tx, empty_rx) = crossbeam_channel::bounded::<Vec<_>>(RECYCLE_BOUND);
    for _ in 0..POOL_SIZE {
        empty_tx
            .send(Vec::with_capacity(BATCH_CAPACITY))
            .expect("recycle channel must accept initial pool");
    }

    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe {
            libc::signal(sig, signal_handler as *const () as libc::sighandler_t);
        }
    }

    let recv_handle = std::thread::spawn(move || -> eyre::Result<u64> {
        let mut recv = Receiver::new(sockets)?;
        let total = recv.run(&SIGNAL_STOP, &full_tx, &empty_rx)?;
        Ok(total)
    });

    let (event_tx, event_rx) = crossbeam_channel::unbounded::<LinkEvent>();
    let nl_names = names.clone();
    let nl_handle = std::thread::spawn(move || netlink::run(&SIGNAL_STOP, &nl_names, &event_tx));

    // Last observed link state per sock_id, so we log only edges.
    let mut link_up: Vec<Option<bool>> = vec![None; names.len()];
    // Last logged bus state per sock_id, so we log only transitions.
    let mut bus_state: Vec<BusState> = vec![BusState::default(); names.len()];

    let retry_activation_failures = config.retry_activation_failures;
    let streams = config
        .streams
        .into_iter()
        .map(|stream| {
            let formatter: Box<dyn Formatter> = match stream.format {
                Format::CandumpFile => {
                    Box::new(CanutilsFileFormatter::new(names.clone(), stream.timestamp))
                }
                Format::CandumpConsole => Box::new(CanutilsConsoleFormatter::new(
                    names.clone(),
                    stream.timestamp,
                )),
            };
            let mut sink = SinkConfig::new(stream.output);
            sink.header = formatter.header().map(|h| h.to_vec());
            sink.compress = stream.compress;
            sink.flush_every = stream.flush_every;
            sink.sync_every = stream.sync_every;
            sink.rotation = stream.rotation;
            sink.retry_activation_failures = retry_activation_failures;
            (formatter, Sink::new(sink))
        })
        .collect();
    let mut pipeline = Pipeline::new(streams);

    // Write-path errors are logged and recorded rather than propagated: returning early would skip
    // draining the remaining batches and closing the pipeline, both of which can lose buffered data.
    // Every error sets `failed` so the process still exits nonzero.
    //
    // A write_batch error means the Sink gave up on the output, so the loop breaks instead of
    // continuing to write into something broken.
    let mut failed = false;

    // Debounce link state and bus state events.
    let mut state_debounce = Debounce::new(Duration::from_millis(200));

    loop {
        select! {
            recv(full_rx) -> msg => match msg {
                Ok(mut batch) => {
                    if log_error_frames(&batch, &mut bus_state, &names) {
                        state_debounce.trigger(Instant::now());
                    }
                    if let Err(e) = pipeline.write_batch(&batch) {
                        if is_broken_pipe(&e) {
                            tracing::debug!("output closed; shutting down");
                            break;
                        }
                        tracing::error!(error = ?e, "failed to write batch");
                        failed = true;
                        break;
                    }
                    batch.clear();
                    let _ = empty_tx.try_send(batch);
                }
                Err(_) => break,
            },
            recv(event_rx) -> msg => match msg {
                Ok(event) => {
                    if handle_link_event(event, &mut link_up, &names) {
                        state_debounce.trigger(Instant::now());
                    }
                }
                Err(_) => break,
            },
            default(Duration::from_millis(100)) => {}
        }
        if state_debounce.expired(Instant::now()) {
            tracing::debug!("syncing after link-down or bus-off");
            if let Err(e) = pipeline.sync() {
                tracing::error!(error = ?e, "link-down or bus-off sync failed");
                failed = true;
            }
        }
        if SIGNAL_ROTATE.swap(false, Ordering::Relaxed)
            && let Err(e) = pipeline.rotate()
        {
            tracing::error!(error = ?e, "failed to rotate on SIGHUP");
            failed = true;
            break;
        }
        if let Err(e) = pipeline.tick() {
            tracing::error!(error = ?e, "periodic flush or sync failed");
            failed = true;
        }
        if SIGNAL_STOP.load(Ordering::Relaxed) {
            break;
        }
    }

    // Set STOP so the receiver exits even if we broke out on a channel disconnect.
    SIGNAL_STOP.store(true, Ordering::Relaxed);

    // Join before draining so we write every received frame.
    match recv_handle.join() {
        Ok(Ok(total)) => tracing::debug!(total_frames = total, "receiver finished"),
        Ok(Err(e)) => {
            tracing::error!(error = ?e, "receiver thread failed");
            failed = true;
        }
        Err(e) => {
            tracing::error!(panic = ?e, "receiver thread panicked");
            failed = true;
        }
    }

    match nl_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = ?e, "netlink thread failed");
            failed = true;
        }
        Err(e) => {
            tracing::error!(panic = ?e, "netlink thread panicked");
            failed = true;
        }
    }

    // Drain everything the receiver queued before it exited. A failure here is the same
    // unrecoverable class as above, so stop rather than retry the same broken output once per
    // queued batch.
    while let Ok(mut batch) = full_rx.try_recv() {
        if let Err(e) = pipeline.write_batch(&batch) {
            if is_broken_pipe(&e) {
                break;
            }
            tracing::error!(error = ?e, "failed to write batch during drain");
            failed = true;
            break;
        }
        batch.clear();
        let _ = empty_tx.try_send(batch);
    }

    // Log any link transitions the netlink thread queued before it exited.
    while let Ok(event) = event_rx.try_recv() {
        handle_link_event(event, &mut link_up, &names);
    }

    // close() always runs, even after write errors: for file and zstd writers it is what writes the
    // epilogue and fsyncs, so skipping it could leave output unrecoverable.
    if let Err(e) = pipeline.close() {
        tracing::error!(error = ?e, "failed to close pipeline");
        failed = true;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
