//! Backend 5: single thread, io_uring multishot `RecvMulti` with provided buffer ring.
//!
//! Requires Linux 6.0+.

use std::alloc::Layout;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use io_uring::types::BufRingEntry;
use io_uring::{IoUring, cqueue, opcode, types};

use crate::can::{CanFrame, FRAME_SIZE};

const RING_ENTRIES: u16 = 256;
const BGID: u16 = 0;
const TIMEOUT_UD: u64 = u64::MAX;

/// Receives CAN frames using io_uring multishot `RecvMulti` with a provided buffer ring.
///
/// The kernel picks buffers from the ring on each completion, avoiding per-recv buffer
/// setup. Multishot means a single SQE generates completions until cancelled or the buffer
/// ring is exhausted.
pub struct UringMultiRecv {
    ring: IoUring,
    sockets: Vec<OwnedFd>,
    br_ptr: *mut u8,
    br_layout: Layout,
    buffers: Vec<u8>,
}

// SAFETY: `br_ptr` points to memory we exclusively own, accessed only from `run()`.
unsafe impl Send for UringMultiRecv {}

impl UringMultiRecv {
    /// Create a new receiver from non-blocking sockets (from
    /// [open_can_raw](crate::can::open_can_raw)).
    pub fn new(sockets: Vec<OwnedFd>) -> std::io::Result<Self> {
        let entries = (sockets.len() as u32 + 1).next_power_of_two().max(4);
        let ring = IoUring::new(entries)?;

        // Page-aligned allocation for the buffer ring descriptor array.
        let br_size = RING_ENTRIES as usize * std::mem::size_of::<BufRingEntry>();
        let br_layout = Layout::from_size_align(br_size, 4096).map_err(std::io::Error::other)?;
        let br_ptr = unsafe { std::alloc::alloc_zeroed(br_layout) };
        if br_ptr.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "buffer ring alloc failed",
            ));
        }

        let buffers = vec![0u8; RING_ENTRIES as usize * FRAME_SIZE];
        let br_base = br_ptr as *mut BufRingEntry;

        // Populate ring entries, each pointing into `buffers`.
        for i in 0..RING_ENTRIES as usize {
            unsafe {
                let entry = &mut *br_base.add(i);
                entry.set_addr(buffers.as_ptr().add(i * FRAME_SIZE) as u64);
                entry.set_len(FRAME_SIZE as u32);
                entry.set_bid(i as u16);
            }
        }

        // Set initial tail = RING_ENTRIES (all buffers available).
        unsafe {
            let tail = BufRingEntry::tail(br_base as *const _) as *mut u16;
            tail.write(RING_ENTRIES);
        }

        // Register the provided buffer ring with the kernel.
        unsafe {
            ring.submitter()
                .register_buf_ring_with_flags(br_ptr as u64, RING_ENTRIES, BGID, 0)?;
        }

        Ok(Self {
            ring,
            sockets,
            br_ptr,
            br_layout,
            buffers,
        })
    }

    /// Run until `stop` is set. Calls `on_frame` for each received frame with the socket index.
    ///
    /// Primes one persistent `RecvMulti` SQE per socket. The kernel selects buffers from the
    /// provided ring on each completion. A 100ms timeout SQE allows checking the stop flag.
    /// Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: Arc<AtomicBool>,
        on_frame: &mut dyn FnMut(usize, &CanFrame),
    ) -> std::io::Result<u64> {
        let mut total = 0u64;
        let ts = types::Timespec::new().nsec(100_000_000);
        let br_base = self.br_ptr as *mut BufRingEntry;
        let mask = RING_ENTRIES - 1;

        // Prime one RecvMulti per socket + one Timeout.
        for (idx, sock) in self.sockets.iter().enumerate() {
            let entry = opcode::RecvMulti::new(types::Fd(sock.as_raw_fd()), BGID)
                .build()
                .user_data(idx as u64);
            unsafe { self.ring.submission().push(&entry) }
                .map_err(|_| std::io::Error::other("SQ full"))?;
        }
        let timeout = opcode::Timeout::new(&ts).build().user_data(TIMEOUT_UD);
        unsafe { self.ring.submission().push(&timeout) }
            .map_err(|_| std::io::Error::other("SQ full"))?;

        while !stop.load(Ordering::Relaxed) {
            self.ring.submit_and_wait(1)?;

            let cqes: Vec<_> = self
                .ring
                .completion()
                .map(|cqe| (cqe.user_data(), cqe.result(), cqe.flags()))
                .collect();

            for &(ud, result, flags) in &cqes {
                if ud == TIMEOUT_UD {
                    let entry = opcode::Timeout::new(&ts).build().user_data(TIMEOUT_UD);
                    unsafe { self.ring.submission().push(&entry) }.ok();
                    continue;
                }

                let idx = ud as usize;

                if result < 0 {
                    let err = std::io::Error::from_raw_os_error(-result);
                    // ECANCELED is normal during shutdown.
                    if err.raw_os_error() != Some(libc::ECANCELED) {
                        return Err(err);
                    }
                } else if let Some(buf_id) = cqueue::buffer_select(flags) {
                    if result == FRAME_SIZE as i32 {
                        let offset = buf_id as usize * FRAME_SIZE;
                        let frame =
                            unsafe { *(self.buffers.as_ptr().add(offset).cast::<CanFrame>()) };
                        on_frame(idx, &frame);
                        total += 1;
                    }

                    // Return the buffer to the ring.
                    self.return_buffer(br_base, buf_id, mask);
                }

                // If MORE flag is absent, the multishot was terminated; resubmit.
                if !cqueue::more(flags) {
                    let entry =
                        opcode::RecvMulti::new(types::Fd(self.sockets[idx].as_raw_fd()), BGID)
                            .build()
                            .user_data(ud);
                    unsafe { self.ring.submission().push(&entry) }.ok();
                }
            }
        }

        Ok(total)
    }

    fn return_buffer(&self, br_base: *mut BufRingEntry, buf_id: u16, mask: u16) {
        unsafe {
            let tail_ptr = BufRingEntry::tail(br_base as *const _) as *const AtomicU16;
            let tail_atomic = &*tail_ptr;
            let tail = tail_atomic.load(Ordering::Relaxed);
            let entry = &mut *br_base.add((tail & mask) as usize);
            let offset = buf_id as usize * FRAME_SIZE;
            entry.set_addr(self.buffers.as_ptr().add(offset) as u64);
            entry.set_len(FRAME_SIZE as u32);
            entry.set_bid(buf_id);
            tail_atomic.store(tail.wrapping_add(1), Ordering::Release);
        }
    }
}

impl Drop for UringMultiRecv {
    fn drop(&mut self) {
        // The kernel cleans up the buf ring registration when the io_uring fd closes.
        // We just free our allocation.
        unsafe {
            std::alloc::dealloc(self.br_ptr, self.br_layout);
        }
    }
}
