//! Backend 2: single thread, `epoll_wait` + `read()`.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::can::{CanFrame, FRAME_SIZE};

/// Receives CAN frames using epoll for readiness notification and `read()` to recv.
pub struct EpollRecv {
    epoll_fd: OwnedFd,
    sockets: Vec<OwnedFd>,
}

impl EpollRecv {
    /// Create a new receiver from non-blocking sockets (from
    /// [open_can_raw](crate::can::open_can_raw)).
    pub fn new(sockets: Vec<OwnedFd>) -> std::io::Result<Self> {
        let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let epoll_fd = unsafe { OwnedFd::from_raw_fd(epoll_fd) };

        for (idx, sock) in sockets.iter().enumerate() {
            let mut ev = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: idx as u64,
            };
            let ret = unsafe {
                libc::epoll_ctl(
                    epoll_fd.as_raw_fd(),
                    libc::EPOLL_CTL_ADD,
                    sock.as_raw_fd(),
                    &mut ev,
                )
            };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }

        Ok(Self { epoll_fd, sockets })
    }

    /// Run until `stop` is set. Calls `on_frame` for each received frame with the socket index.
    ///
    /// Uses `epoll_wait` with a 100ms timeout, then drains all ready sockets with `read()`.
    /// Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: Arc<AtomicBool>,
        on_frame: &mut dyn FnMut(usize, &CanFrame),
    ) -> std::io::Result<u64> {
        let mut events = [unsafe { std::mem::zeroed::<libc::epoll_event>() }; 64];
        let mut total = 0u64;

        while !stop.load(Ordering::Relaxed) {
            let n = unsafe {
                libc::epoll_wait(
                    self.epoll_fd.as_raw_fd(),
                    events.as_mut_ptr(),
                    events.len() as i32,
                    100,
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }

            for ev in &events[..n as usize] {
                let idx = ev.u64 as usize;
                let sock_fd = self.sockets[idx].as_raw_fd();

                // Drain all available frames from this socket.
                loop {
                    let mut frame = CanFrame::default();
                    let n = unsafe {
                        libc::read(
                            sock_fd,
                            std::ptr::from_mut(&mut frame).cast::<libc::c_void>(),
                            FRAME_SIZE,
                        )
                    };
                    if n == FRAME_SIZE as isize {
                        on_frame(idx, &frame);
                        total += 1;
                    } else if n < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() != Some(libc::EAGAIN) {
                            return Err(err);
                        }
                        break;
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(total)
    }
}
