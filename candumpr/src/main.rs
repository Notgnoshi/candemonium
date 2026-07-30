use std::os::unix::io::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use candumpr::can;
use candumpr::debounce::Debounce;
use candumpr::errframe::{BusState, ErrorFrame};
use candumpr::format::{CanutilsConsoleFormatter, CanutilsFileFormatter, Formatter, TimestampMode};
use candumpr::frame::CanFrame;
use candumpr::pipeline::Pipeline;
use candumpr::recv::netlink::{self, LinkEvent};
use candumpr::recv::receiver::{BATCH_CAPACITY, Receiver};
use candumpr::sink::{Output, Sink, SinkConfig};
use clap::Parser;
use crossbeam_channel::select;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
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

/// The first interface name that appears more than once.
fn first_duplicate(interfaces: &[String]) -> Option<&str> {
    let mut seen = std::collections::HashSet::new();
    interfaces
        .iter()
        .find(|name| !seen.insert(name.as_str()))
        .map(String::as_str)
}

/// Output format for received frames.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Format {
    /// can-utils candump file format: `(ts) iface ID#DATA`.
    CandumpFile,
    /// can-utils candump console format: `(ts) iface ID [len] B0 B1 ...`.
    CandumpConsole,
}

impl Format {
    /// Log file extension for each output format
    fn ext(&self) -> &'static str {
        match self {
            Format::CandumpFile => "log",
            Format::CandumpConsole => "txt",
        }
    }
}

/// Log CAN traffic from multiple networks.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// CAN interfaces to listen on.
    #[arg(required = true)]
    interfaces: Vec<String>,

    /// Log each interface to its own file in the current directory, instead of stdout.
    #[arg(long, short = 'l', conflicts_with = "output")]
    log: bool,

    /// Log to this file path. Truncated if it already exists.
    #[arg(long, short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,

    /// Output format for received frames.
    #[arg(long, value_enum, default_value = "candump-file")]
    format: Format,

    /// Timestamp rendering mode. Only applies to the candump formats.
    #[arg(long, value_enum, default_value = "absolute")]
    timestamp: TimestampMode,

    /// Log level for tracing output on stderr.
    #[arg(long, default_value = "INFO")]
    log_level: tracing::Level,
}

fn main() -> ExitCode {
    if let Err(e) = color_eyre::install() {
        eprintln!("failed to install error handler: {e:#}");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(cli.log_level)
        .init();

    // A repeated interface would log every frame on it twice, and in --log mode its two Sinks would
    // race to create and truncate the same scheme-named file.
    if let Some(duplicate) = first_duplicate(&cli.interfaces) {
        tracing::error!(interface = %duplicate, "interface given more than once");
        return ExitCode::FAILURE;
    }

    // The sockets vector defines the canonical interface ordering. The orderings of:
    //
    // 1. cli.interfaces
    // 2. sockets
    // 3. Pipeline::sinks / bufs
    // 4. et al.
    //
    // all follow the same ordering, and are all indexed by CanFrame::sock_id
    let sockets: Vec<_> = match cli
        .interfaces
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

    for (name, sock) in cli.interfaces.iter().zip(&sockets) {
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

    for sig in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            libc::signal(sig, signal_handler as *const () as libc::sighandler_t);
        }
    }

    let recv_handle = std::thread::spawn(move || -> eyre::Result<u64> {
        let mut recv = Receiver::new(sockets)?;
        let total = recv.run(&STOP, &full_tx, &empty_rx)?;
        Ok(total)
    });

    let (event_tx, event_rx) = crossbeam_channel::unbounded::<LinkEvent>();
    let nl_names = cli.interfaces.clone();
    let nl_handle = std::thread::spawn(move || netlink::run(&STOP, &nl_names, &event_tx));

    // Names indexed by sock_id, kept for logging link and bus transitions on the main thread.
    let names = cli.interfaces.clone();
    // Last observed link state per sock_id, so we log only edges.
    let mut link_up: Vec<Option<bool>> = vec![None; names.len()];
    // Last logged bus state per sock_id, so we log only transitions.
    let mut bus_state: Vec<BusState> = vec![BusState::default(); names.len()];

    let make_formatter = || -> Box<dyn Formatter> {
        match cli.format {
            Format::CandumpFile => {
                Box::new(CanutilsFileFormatter::new(names.clone(), cli.timestamp))
            }
            Format::CandumpConsole => {
                Box::new(CanutilsConsoleFormatter::new(names.clone(), cli.timestamp))
            }
        }
    };

    // --log gives every interface its own output, so its traffic lands in its own file. Everything
    // else interleaves all interfaces into one output.
    let outputs: Vec<Output> = if cli.log {
        names
            .iter()
            .map(|interface| Output::Template {
                dir: ".".into(),
                interface: interface.clone(),
                ext: cli.format.ext().to_string(),
                next_index: 0,
            })
            .collect()
    } else if let Some(path) = cli.output {
        vec![Output::Path(path)]
    } else {
        vec![Output::Stdout]
    };
    let streams = outputs
        .into_iter()
        .map(|output| {
            let formatter = make_formatter();
            let mut config = SinkConfig::new(output);
            config.header = formatter.header().map(|h| h.to_vec());
            (formatter, Sink::new(config))
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
        if let Err(e) = pipeline.tick() {
            tracing::error!(error = ?e, "periodic flush or sync failed");
            failed = true;
        }
        if STOP.load(Ordering::Relaxed) {
            break;
        }
    }

    // Set STOP so the receiver exits even if we broke out on a channel disconnect.
    STOP.store(true, Ordering::Relaxed);

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
