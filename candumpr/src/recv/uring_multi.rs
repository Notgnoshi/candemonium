//! Backend 5: single thread, io_uring multishot `RecvMulti` with provided buffer ring.
//!
//! Requires Linux 6.1+.
//!
//! Performance features:
//! - SINGLE_ISSUER: skip internal synchronization (single-threaded use).
//! - COOP_TASKRUN: prevent kernel from delivering task_work at arbitrary syscall boundaries.
//! - DEFER_TASKRUN: defer all completion processing to explicit submit_with_args calls.
//! - Registered file descriptors: avoid per-operation fd lookup in the kernel.
//! - Batched wakeups: submit_with_args(BATCH_SIZE) reduces wakeup frequency.
//! - Enlarged CQ ring: headroom for burst-induced multishot completions.

use std::alloc::Layout;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use io_uring::types::BufRingEntry;
use io_uring::{IoUring, cqueue, opcode, types};

use crate::can::{CanFrame, FRAME_SIZE};

/// Size of the ring buffer that holds the output CAN frames
const FRAMEBUF_COUNT: u16 = 256;

/// Buffer group ID for the provided buffer ring. io_uring supports multiple
/// buffer rings identified by group ID; we only use one.
const BGID: u16 = 0;

/// Number of CQEs to wait for before waking. Reduces wakeup frequency at the
/// cost of added recv latency. The 100ms timeout on submit_with_args ensures
/// we still wake promptly for shutdown or when traffic is sparse.
const BATCH_SIZE: usize = 4;

/// CQ ring size. With multishot recv, a single SQE can generate many CQEs in a
/// burst. A larger CQ ring prevents overflow (which terminates the multishot
/// and forces resubmission).
const CQ_SIZE: u32 = 64;

/// Receives CAN frames using io_uring multishot `RecvMulti` with a provided buffer ring.
///
/// The kernel picks buffers from the ring on each completion, avoiding per-recv buffer
/// setup. Multishot means a single SQE generates completions until cancelled or the buffer
/// ring is exhausted.
pub struct UringMultiRecv {
    ring: IoUring,
    sockets: Vec<OwnedFd>,
    // Array of BufRingEntry descriptors, which point into framebuf_data
    framebuf_ring_ptr: *mut u8,
    framebuf_ring_layout: Layout,
    framebuf_data: Box<[u8]>,
}

// SAFETY: `framebuf_ring_ptr` points to memory we exclusively own, accessed only from `run()`.
unsafe impl Send for UringMultiRecv {}

impl UringMultiRecv {
    /// Create a new receiver from non-blocking sockets (from
    /// [open_can_raw](crate::can::open_can_raw)).
    pub fn new(sockets: Vec<OwnedFd>) -> std::io::Result<Self> {
        // SQ size: one slot per socket for the initial RecvMulti SQEs, plus
        // headroom for resubmissions when multishot terminates. io_uring
        // requires a power of two. CQ size is set separately via setup_cqsize.
        let sq_size = (sockets.len() as u32).next_power_of_two().max(4);
        let ring = IoUring::builder()
            .setup_single_issuer()
            .setup_coop_taskrun()
            .setup_defer_taskrun()
            .setup_cqsize(CQ_SIZE)
            .build(sq_size)?;

        // Register socket file descriptors so the kernel can skip per-op fd
        // lookup. SQEs then use types::Fixed(idx) instead of types::Fd(raw_fd).
        let raw_fds: Vec<RawFd> = sockets.iter().map(|s| s.as_raw_fd()).collect();
        ring.submitter().register_files(&raw_fds)?;

        // Page-aligned allocation for the buffer ring descriptor array. We use
        // raw alloc/dealloc (rather than Box) because the kernel requires this
        // array to be page-aligned, and page size is a runtime value.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let framebuf_ring_size = FRAMEBUF_COUNT as usize * std::mem::size_of::<BufRingEntry>();
        let framebuf_ring_layout = Layout::from_size_align(framebuf_ring_size, page_size)
            .map_err(std::io::Error::other)?;
        let framebuf_ring_ptr = unsafe { std::alloc::alloc_zeroed(framebuf_ring_layout) };
        if framebuf_ring_ptr.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "buffer ring alloc failed",
            ));
        }

        let framebuf_data: Box<[u8]> = vec![0u8; FRAMEBUF_COUNT as usize * FRAME_SIZE].into();
        let framebuf_base = framebuf_ring_ptr as *mut BufRingEntry;

        // Populate ring entries, each pointing into `framebuf_data`.
        for i in 0..FRAMEBUF_COUNT as usize {
            unsafe {
                let entry = &mut *framebuf_base.add(i);
                entry.set_addr(framebuf_data.as_ptr().add(i * FRAME_SIZE) as u64);
                entry.set_len(FRAME_SIZE as u32);
                entry.set_bid(i as u16);
            }
        }

        // Set initial tail = FRAMEBUF_COUNT (all buffers available).
        unsafe {
            let tail = BufRingEntry::tail(framebuf_base as *const _) as *mut u16;
            tail.write(FRAMEBUF_COUNT);
        }

        unsafe {
            ring.submitter().register_buf_ring_with_flags(
                framebuf_ring_ptr as u64,
                FRAMEBUF_COUNT,
                BGID,
                0,
            )?;
        }

        Ok(Self {
            ring,
            sockets,
            framebuf_ring_ptr,
            framebuf_ring_layout,
            framebuf_data,
        })
    }

    /// Run until `stop` is set. Calls `on_frame` for each received frame with the socket index.
    ///
    /// Primes one persistent `RecvMulti` SQE per socket. The kernel selects buffers from the
    /// provided ring on each completion. Uses submit_with_args with a 100ms timeout to
    /// batch wakeups and check the stop flag. Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: Arc<AtomicBool>,
        on_frame: &mut dyn FnMut(usize, &CanFrame),
    ) -> std::io::Result<u64> {
        let mut total = 0u64;
        let timeout = types::Timespec::new().nsec(100_000_000);
        let args = types::SubmitArgs::new().timespec(&timeout);
        let framebuf_base = self.framebuf_ring_ptr as *mut BufRingEntry;
        let mask = FRAMEBUF_COUNT - 1;

        // Prime one RecvMulti per socket using registered fd indices.
        for idx in 0..self.sockets.len() {
            let entry = opcode::RecvMulti::new(types::Fixed(idx as u32), BGID)
                .build()
                .user_data(idx as u64);
            unsafe { self.ring.submission().push(&entry) }
                .map_err(|_| std::io::Error::other("SQ full"))?;
        }

        while !stop.load(Ordering::Relaxed) {
            // Wait for BATCH_SIZE completions or 100ms timeout, whichever comes
            // first. The timeout ensures we check the stop flag and handle
            // partial batches (e.g., the last few frames before shutdown).
            match self.ring.submitter().submit_with_args(BATCH_SIZE, &args) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::ETIME) => {}
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => return Err(e),
            }

            // Drain CQEs into a stack buffer, then process. This avoids heap
            // allocation while releasing the borrow on the completion queue
            // before we need to touch the submission queue or buffer ring.
            let mut cqe_buf = [(0u64, 0i32, 0u32); CQ_SIZE as usize];
            let mut cqe_count = 0;
            for cqe in self.ring.completion() {
                cqe_buf[cqe_count] = (cqe.user_data(), cqe.result(), cqe.flags());
                cqe_count += 1;
            }

            for &(ud, result, flags) in &cqe_buf[..cqe_count] {
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
                        let frame = unsafe {
                            *(self.framebuf_data.as_ptr().add(offset).cast::<CanFrame>())
                        };
                        on_frame(idx, &frame);
                        total += 1;
                    }

                    // Return the buffer to the ring.
                    self.return_buffer(framebuf_base, buf_id, mask);
                }

                // If MORE flag is absent, the multishot was terminated; resubmit.
                if !cqueue::more(flags) {
                    let entry = opcode::RecvMulti::new(types::Fixed(idx as u32), BGID)
                        .build()
                        .user_data(ud);
                    unsafe { self.ring.submission().push(&entry) }.ok();
                }
            }
        }

        Ok(total)
    }

    fn return_buffer(&self, framebuf_base: *mut BufRingEntry, buf_id: u16, mask: u16) {
        unsafe {
            let tail_ptr = BufRingEntry::tail(framebuf_base as *const _) as *const AtomicU16;
            let tail_atomic = &*tail_ptr;
            let tail = tail_atomic.load(Ordering::Relaxed);
            let entry = &mut *framebuf_base.add((tail & mask) as usize);
            let offset = buf_id as usize * FRAME_SIZE;
            entry.set_addr(self.framebuf_data.as_ptr().add(offset) as u64);
            entry.set_len(FRAME_SIZE as u32);
            entry.set_bid(buf_id);
            tail_atomic.store(tail.wrapping_add(1), Ordering::Release);
        }
    }
}

impl Drop for UringMultiRecv {
    fn drop(&mut self) {
        // The kernel cleans up the buf ring registration when the io_uring fd closes. We just free
        // our allocation.
        unsafe {
            std::alloc::dealloc(self.framebuf_ring_ptr, self.framebuf_ring_layout);
        }
    }
}
