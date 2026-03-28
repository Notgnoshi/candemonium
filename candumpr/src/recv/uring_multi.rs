//! Backend 5: single thread, io_uring multishot `RecvMsgMulti` with provided buffer ring.
//!
//! Requires Linux 6.1+.
//!
//! Performance features:
//! * SINGLE_ISSUER: skip internal synchronization (single-threaded use).
//! * COOP_TASKRUN: prevent kernel from delivering task_work at arbitrary syscall boundaries.
//! * DEFER_TASKRUN: defer all completion processing to explicit submit_with_args calls.
//! * Registered file descriptors: avoid per-operation fd lookup in the kernel.
//! * Batched wakeups: submit_with_args(BATCH_SIZE) reduces wakeup frequency.
//!
//! Ancillary data:
//! * Hardware timestamps (SCM_TIMESTAMPING) with software fallback.
//! * Kernel drop count (SO_RXQ_OVFL).

use std::alloc::Layout;
use std::os::unix::io::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use io_uring::types::BufRingEntry;
use io_uring::{IoUring, cqueue, opcode, types};

use crate::can::{self, CanFrame, FRAME_SIZE};
use crate::recv::{FrameMeta, Timestamp};

/// Number of provided buffers (and CQ entries) in the ring. The CQ is sized to match so the
/// kernel can post one completion per buffer without overflow. Must be a power of two.
const FRAMEBUF_COUNT: u16 = 256;
const _: () = assert!(FRAMEBUF_COUNT.is_power_of_two());

/// Buffer group ID for the provided buffer ring. io_uring supports multiple buffer rings
/// identified by group ID; we only use one.
const BGID: u16 = 0;

/// Number of CQEs to wait for before waking. Reduces wakeup frequency at the cost of added recv
/// latency. The 100ms timeout on submit_with_args ensures we still wake promptly for shutdown or
/// when traffic is sparse.
const BATCH_SIZE: usize = 4;

/// Size of the `io_uring_recvmsg_out` header the kernel writes at the start of each provided
/// buffer. This is a stable kernel ABI (4 x u32).
const RECVMSG_OUT_HDR: usize = 16;

/// Space reserved for control messages (cmsg) in each provided buffer. Must fit SCM_TIMESTAMPING
/// (cmsghdr + 3 timespecs, ~64 bytes) and SO_RXQ_OVFL (cmsghdr + u32, ~24 bytes). 128 bytes gives
/// headroom.
const CMSG_BUF_SIZE: usize = 128;

/// Total size of each provided buffer entry.
/// Layout: `[recvmsg_out header][control data][CAN frame payload]`
/// (msg_namelen is 0, so no name section.)
const BUF_ENTRY_SIZE: usize = RECVMSG_OUT_HDR + CMSG_BUF_SIZE + FRAME_SIZE;

/// Receives CAN frames using io_uring multishot `RecvMsgMulti` with a provided
/// buffer ring.
///
/// Uses `recvmsg` (rather than plain `recv`) to access ancillary data (hardware timestamps and
/// kernel drop counts). The kernel picks buffers from the ring on each completion. Multishot means
/// a single SQE generates completions until cancelled or the buffer ring is exhausted.
pub struct UringMultiRecv {
    ring: IoUring,
    sockets: Vec<OwnedFd>,
    /// Array of BufRingEntry descriptors, page-aligned. Each descriptor points into a slot in
    /// `framebuf_data`. Allocated with raw alloc because the kernel requires page alignment and
    /// page size is a runtime value.
    framebuf_ring_ptr: *mut u8,
    framebuf_ring_layout: Layout,
    /// Contiguous backing memory for provided buffers. Each slot is BUF_ENTRY_SIZE bytes and holds
    /// the recvmsg output (header + cmsg + payload).
    framebuf_data: Box<[u8]>,
}

// SAFETY: `framebuf_ring_ptr` points to memory we exclusively own, accessed only from `run()`.
unsafe impl Send for UringMultiRecv {}

impl UringMultiRecv {
    /// Create a new receiver from non-blocking sockets (from
    /// [open_can_raw](crate::can::open_can_raw)).
    ///
    /// Enables hardware timestamping and drop count reporting on each socket.
    pub fn new(sockets: Vec<OwnedFd>) -> std::io::Result<Self> {
        // Enable ancillary data on each socket before we start receiving.
        for sock in &sockets {
            can::enable_timestamps(sock.as_fd())?;
            can::enable_drop_count(sock.as_fd())?;
        }

        // SQ size: one slot per socket for the initial RecvMsgMulti SQEs, plus headroom for
        // resubmissions when multishot terminates. io_uring requires a power of two. CQ size is
        // set separately via setup_cqsize.
        let sq_size = (sockets.len() as u32).next_power_of_two().max(4);
        let ring = IoUring::builder()
            .setup_single_issuer()
            .setup_coop_taskrun()
            .setup_defer_taskrun()
            .setup_cqsize(FRAMEBUF_COUNT as u32)
            .build(sq_size)?;

        // Register socket file descriptors so the kernel can skip per-op fd lookup. SQEs then use
        // types::Fixed(idx) instead of types::Fd(raw_fd).
        let raw_fds: Vec<RawFd> = sockets.iter().map(|s| s.as_raw_fd()).collect();
        ring.submitter().register_files(&raw_fds)?;

        // Page-aligned allocation for the buffer ring descriptor array. We use raw alloc/dealloc
        // (rather than Box) because the kernel requires this array to be page-aligned, and page
        // size is a runtime value.
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

        let framebuf_data: Box<[u8]> = vec![0u8; FRAMEBUF_COUNT as usize * BUF_ENTRY_SIZE].into();
        let framebuf_base = framebuf_ring_ptr as *mut BufRingEntry;

        // Populate ring entries, each pointing into `framebuf_data`.
        for i in 0..FRAMEBUF_COUNT as usize {
            unsafe {
                let entry = &mut *framebuf_base.add(i);
                entry.set_addr(framebuf_data.as_ptr().add(i * BUF_ENTRY_SIZE) as u64);
                entry.set_len(BUF_ENTRY_SIZE as u32);
                entry.set_bid(i as u16);
            }
        }

        // Set initial tail = FRAMEBUF_COUNT (all buffers available).
        unsafe {
            let tail = BufRingEntry::tail(framebuf_base as *const _) as *mut u16;
            tail.write(FRAMEBUF_COUNT);
        }

        // Register the provided buffer ring with the kernel.
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

    /// Run until `stop` is set. Calls `on_frame` for each received frame with the socket index,
    /// the frame, and metadata (timestamp + drop count).
    ///
    /// Primes one persistent `RecvMsgMulti` SQE per socket. The kernel selects buffers from the
    /// provided ring on each completion and writes the recvmsg output (header + cmsg + payload)
    /// into the buffer. Uses submit_with_args with a 100ms timeout to batch wakeups and check the
    /// stop flag. Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: Arc<AtomicBool>,
        on_frame: &mut dyn FnMut(usize, &CanFrame, &FrameMeta),
    ) -> std::io::Result<u64> {
        let mut total = 0u64;
        let timeout = types::Timespec::new().nsec(100_000_000);
        let args = types::SubmitArgs::new().timespec(&timeout);
        let framebuf_base = self.framebuf_ring_ptr as *mut BufRingEntry;
        let mask = FRAMEBUF_COUNT - 1;

        // Sockets whose multishot terminated but could not be resubmitted because the SQ was full.
        // Retried at the top of each loop iteration after submit drains the SQ.
        let mut pending_resubmit: Vec<usize> = Vec::new();

        // Template msghdr for RecvMsgMulti. The kernel uses msg_namelen and msg_controllen to
        // determine the layout within each provided buffer. Must remain at a stable address for
        // the lifetime of the multishot SQEs (i.e., until this function returns).
        let mut msghdr: libc::msghdr = unsafe { std::mem::zeroed() };
        msghdr.msg_controllen = CMSG_BUF_SIZE;

        // Prime one RecvMsgMulti per socket using registered fd indices.
        for idx in 0..self.sockets.len() {
            let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                .build()
                .user_data(idx as u64);
            unsafe { self.ring.submission().push(&entry) }
                .map_err(|_| std::io::Error::other("SQ full"))?;
        }

        while !stop.load(Ordering::Relaxed) {
            // Wait for BATCH_SIZE completions or 100ms timeout, whichever comes first. The timeout
            // ensures we check the stop flag and handle partial batches (e.g., the last few frames
            // before shutdown).
            match self.ring.submitter().submit_with_args(BATCH_SIZE, &args) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::ETIME) => {}
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => return Err(e),
            }

            // Retry any multishot resubmissions that failed on a previous iteration because the
            // SQ was full. The submit_with_args above drained the SQ, so there should be room now.
            pending_resubmit.retain(|&idx| {
                let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                    .build()
                    .user_data(idx as u64);
                unsafe { self.ring.submission().push(&entry) }.is_err()
            });

            // Drain CQEs into a stack buffer, then process. This avoids heap allocation while
            // releasing the borrow on the completion queue before we need to touch the submission
            // queue or buffer ring.
            let mut cqe_buf = [(0u64, 0i32, 0u32); FRAMEBUF_COUNT as usize];
            let mut cqe_count = 0;
            for cqe in self.ring.completion() {
                cqe_buf[cqe_count] = (cqe.user_data(), cqe.result(), cqe.flags());
                cqe_count += 1;
            }

            for &(ud, result, flags) in &cqe_buf[..cqe_count] {
                let idx = ud as usize;

                if result < 0 {
                    let err_code = -result;
                    // ECANCELED: normal shutdown (SQE cancelled).
                    // ENOBUFS: provided buffer ring exhausted; multishot terminated. The
                    // resubmission logic below will restart it once buffers are returned.
                    if err_code != libc::ECANCELED && err_code != libc::ENOBUFS {
                        return Err(std::io::Error::from_raw_os_error(err_code));
                    }
                } else if let Some(buf_id) = cqueue::buffer_select(flags) {
                    let buf_offset = buf_id as usize * BUF_ENTRY_SIZE;
                    let buf = &self.framebuf_data[buf_offset..buf_offset + BUF_ENTRY_SIZE];

                    if let Ok(out) = types::RecvMsgOut::parse(buf, &msghdr) {
                        let payload = out.payload_data();
                        if payload.len() == FRAME_SIZE {
                            let frame = unsafe {
                                std::ptr::read_unaligned(payload.as_ptr().cast::<CanFrame>())
                            };
                            let meta = parse_control_data(out.control_data());
                            on_frame(idx, &frame, &meta);
                            total += 1;
                        }
                    }

                    self.return_buffer(framebuf_base, buf_id, mask);
                }

                // If MORE flag is absent, the multishot was terminated; resubmit.
                if !cqueue::more(flags) {
                    let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                        .build()
                        .user_data(ud);
                    if unsafe { self.ring.submission().push(&entry) }.is_err() {
                        pending_resubmit.push(idx);
                    }
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
            let offset = buf_id as usize * BUF_ENTRY_SIZE;
            entry.set_addr(self.framebuf_data.as_ptr().add(offset) as u64);
            entry.set_len(BUF_ENTRY_SIZE as u32);
            entry.set_bid(buf_id);
            tail_atomic.store(tail.wrapping_add(1), Ordering::Release);
        }
    }
}

impl Drop for UringMultiRecv {
    fn drop(&mut self) {
        // Unregister the buffer ring from the kernel before freeing it. Our explicit Drop runs
        // before field drops, so `self.ring` (IoUring) is still alive here. Without unregistering
        // first, the kernel might access the freed buffer ring memory during io_uring fd cleanup.
        let _ = self.ring.submitter().unregister_buf_ring(BGID);
        unsafe {
            std::alloc::dealloc(self.framebuf_ring_ptr, self.framebuf_ring_layout);
        }
    }
}

/// Parse the cmsg chain from recvmsg control data into [FrameMeta].
///
/// Walks the control buffer looking for:
/// * `SCM_TIMESTAMPING`: 3 timespecs (software, deprecated, hw_raw). Prefers hw_raw (index 2),
///   falls back to software (index 0).
/// * `SO_RXQ_OVFL`: cumulative u32 drop count since socket creation.
fn parse_control_data(control: &[u8]) -> FrameMeta {
    let mut meta = FrameMeta::default();
    let hdr_size = std::mem::size_of::<libc::cmsghdr>();
    let align = std::mem::align_of::<libc::cmsghdr>();
    let mut offset = 0;

    while offset + hdr_size <= control.len() {
        let hdr = unsafe { &*(control.as_ptr().add(offset) as *const libc::cmsghdr) };
        let cmsg_len = hdr.cmsg_len;
        if cmsg_len < hdr_size {
            break;
        }

        let data_start = offset + hdr_size;
        let data_len = cmsg_len - hdr_size;

        if hdr.cmsg_level == libc::SOL_SOCKET && data_start + data_len <= control.len() {
            match hdr.cmsg_type {
                libc::SCM_TIMESTAMPING => {
                    // scm_timestamping: [software, hw_transformed (deprecated), hw_raw]
                    let ts_size = std::mem::size_of::<libc::timespec>();
                    if data_len >= 3 * ts_size {
                        let ts = unsafe {
                            std::slice::from_raw_parts(
                                control.as_ptr().add(data_start) as *const libc::timespec,
                                3,
                            )
                        };
                        // Prefer hw_raw (index 2), fall back to software (index 0).
                        let chosen = if ts[2].tv_sec != 0 || ts[2].tv_nsec != 0 {
                            &ts[2]
                        } else {
                            &ts[0]
                        };
                        if chosen.tv_sec != 0 || chosen.tv_nsec != 0 {
                            meta.timestamp = Some(Timestamp {
                                sec: chosen.tv_sec,
                                nsec: chosen.tv_nsec,
                            });
                        }
                    }
                }
                libc::SO_RXQ_OVFL => {
                    if data_len >= std::mem::size_of::<u32>() {
                        meta.drops = Some(unsafe {
                            std::ptr::read_unaligned(control.as_ptr().add(data_start) as *const u32)
                        });
                    }
                }
                _ => {}
            }
        }

        // Advance to next cmsg: CMSG_ALIGN(cmsg_len).
        offset += (cmsg_len + align - 1) & !(align - 1);
    }

    meta
}
