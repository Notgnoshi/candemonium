//! Backend 1: one dedicated thread per socket, blocking `read()`.

use std::os::unix::io::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::can::{CanFrame, FRAME_SIZE};

/// Receives CAN frames using one blocking thread per socket.
pub struct DedicatedRecv {
    sockets: Vec<OwnedFd>,
}

impl DedicatedRecv {
    /// Create a new receiver from blocking sockets (from
    /// [open_can_raw_blocking](crate::can::open_can_raw_blocking)).
    pub fn new(sockets: Vec<OwnedFd>) -> Self {
        Self { sockets }
    }

    /// Run until `stop` is set. Calls `on_frame` for each received frame with the socket index.
    ///
    /// Spawns one scoped thread per socket. Each socket has `SO_RCVTIMEO` set to 100ms so
    /// threads can periodically check the stop flag. Returns the total number of frames
    /// received across all threads.
    pub fn run(
        self,
        stop: Arc<AtomicBool>,
        on_frame: &(dyn Fn(usize, &CanFrame) + Send + Sync),
    ) -> std::io::Result<u64> {
        // Can't block indefinitely; we need to be able to terminate the threads.
        for fd in &self.sockets {
            set_rcvtimeo(fd, 0, 100_000)?;
        }

        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .sockets
                .into_iter()
                .enumerate()
                .map(|(idx, fd)| {
                    let stop = stop.clone();
                    scope.spawn(move || -> std::io::Result<u64> {
                        let mut count = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            let mut frame = CanFrame::default();
                            let n = unsafe {
                                libc::read(
                                    fd.as_raw_fd(),
                                    std::ptr::from_mut(&mut frame).cast::<libc::c_void>(),
                                    FRAME_SIZE,
                                )
                            };
                            if n == FRAME_SIZE as isize {
                                on_frame(idx, &frame);
                                count += 1;
                            } else if n < 0 {
                                let err = std::io::Error::last_os_error();
                                match err.raw_os_error() {
                                    Some(libc::EAGAIN | libc::ETIMEDOUT) => {}
                                    _ => return Err(err),
                                }
                            }
                        }
                        Ok(count)
                    })
                })
                .collect();

            let mut total = 0u64;
            for h in handles {
                total += h.join().unwrap()?;
            }
            Ok(total)
        })
    }
}

fn set_rcvtimeo(fd: &OwnedFd, secs: i64, usecs: i64) -> std::io::Result<()> {
    let tv = libc::timeval {
        tv_sec: secs,
        tv_usec: usecs,
    };
    let ret = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::from_ref(&tv).cast::<libc::c_void>(),
            std::mem::size_of::<libc::timeval>() as u32,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
