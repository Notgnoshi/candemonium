//! Backend 3: single thread, `epoll_wait` + `recvmmsg()`.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::can::{CanFrame, FRAME_SIZE};

const BATCH_SIZE: usize = 32;

/// Receives CAN frames using epoll + `recvmmsg` for batch recv.
pub struct RecvmmsgRecv {
    epoll_fd: OwnedFd,
    sockets: Vec<OwnedFd>,
}

impl RecvmmsgRecv {
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
    /// Uses `epoll_wait` with a 100ms timeout, then calls `recvmmsg` on each ready socket to
    /// receive up to 32 frames at once. Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: Arc<AtomicBool>,
        on_frame: &mut dyn FnMut(usize, &CanFrame),
    ) -> std::io::Result<u64> {
        let mut events = [unsafe { std::mem::zeroed::<libc::epoll_event>() }; 64];
        let mut total = 0u64;

        // Pre-allocate batch buffers.
        let mut frames = [CanFrame::default(); BATCH_SIZE];
        let mut iovecs: [libc::iovec; BATCH_SIZE] = unsafe { std::mem::zeroed() };
        let mut msghdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { std::mem::zeroed() };

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

                // Drain with recvmmsg in batches.
                loop {
                    // Zero frames and set up iovecs pointing into them.
                    frames.fill(CanFrame::default());
                    for i in 0..BATCH_SIZE {
                        iovecs[i].iov_base =
                            std::ptr::from_mut(&mut frames[i]).cast::<libc::c_void>();
                        iovecs[i].iov_len = FRAME_SIZE;
                        msghdrs[i].msg_hdr.msg_iov = &mut iovecs[i];
                        msghdrs[i].msg_hdr.msg_iovlen = 1;
                        msghdrs[i].msg_len = 0;
                    }

                    let received = unsafe {
                        libc::recvmmsg(
                            sock_fd,
                            msghdrs.as_mut_ptr(),
                            BATCH_SIZE as u32,
                            libc::MSG_DONTWAIT,
                            std::ptr::null_mut(),
                        )
                    };
                    if received < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() != Some(libc::EAGAIN) {
                            return Err(err);
                        }
                        break;
                    }
                    if received == 0 {
                        break;
                    }

                    for i in 0..received as usize {
                        if msghdrs[i].msg_len == FRAME_SIZE as u32 {
                            on_frame(idx, &frames[i]);
                            total += 1;
                        }
                    }

                    // If we got fewer than a full batch, the socket is drained.
                    if (received as usize) < BATCH_SIZE {
                        break;
                    }
                }
            }
        }

        Ok(total)
    }
}
