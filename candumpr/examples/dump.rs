//! Listen on CAN interfaces using the io_uring multishot backend and print received frames.
//!
//! Usage: uring_multi_dump <iface> [iface...]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use candumpr::can::{self, CanFrame};
use candumpr::recv::uring_multi::UringMultiRecv;

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let ifaces: Vec<String> = std::env::args().skip(1).collect();
    if ifaces.is_empty() {
        eprintln!("usage: uring_multi_dump <iface> [iface...]");
        std::process::exit(1);
    }

    let sockets: Vec<_> = ifaces
        .iter()
        .map(|name| can::open_can_raw(name))
        .collect::<std::io::Result<_>>()?;

    let mut backend = UringMultiRecv::new(sockets)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    ctrlc(stop2);

    let total = backend.run(stop, &mut |idx, frame, _meta| {
        print_frame(idx, frame);
    })?;

    eprintln!("{total} frames received");
    Ok(())
}

fn print_frame(idx: usize, frame: &CanFrame) {
    let id = frame.can_id & !libc::CAN_EFF_FLAG & !libc::CAN_RTR_FLAG & !libc::CAN_ERR_FLAG;

    print!("{idx} {id:08X} [{}]", frame.len);
    for i in 0..frame.len as usize {
        print!(" {:02X}", frame.data[i]);
    }
    println!();
}

/// Install a Ctrl-C handler that sets the stop flag.
fn ctrlc(stop: Arc<AtomicBool>) {
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
    }
    // Leak the Arc into a raw pointer so the signal handler can access it.
    STOP_FLAG.store(Arc::into_raw(stop) as *mut _, Ordering::Release);
}

static STOP_FLAG: std::sync::atomic::AtomicPtr<AtomicBool> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

extern "C" fn signal_handler(_sig: libc::c_int) {
    let ptr = STOP_FLAG.load(Ordering::Acquire);
    if !ptr.is_null() {
        unsafe { &*ptr }.store(true, Ordering::Relaxed);
    }
}
