use std::alloc::Layout;
use std::os::unix::io::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use io_uring::types::BufRingEntry;
use io_uring::{IoUring, cqueue, opcode, types};

use crate::can::{self, FRAME_SIZE, LinuxCanFrame};
use crate::frame::{CanFrame, Direction};
use crate::recv::Timestamp;

/// Number of provided buffers (and CQ entries) in the ring. Must be a power of two.
const FRAMEBUF_COUNT: u16 = 256;
const _: () = assert!(FRAMEBUF_COUNT.is_power_of_two());

/// Capacity of a batch Vec sent over the SPSC channel. Sized to match the maximum number of
/// CQEs that can be drained in one cycle.
pub const BATCH_CAPACITY: usize = FRAMEBUF_COUNT as usize;

const BGID: u16 = 0;

/// Number of CQEs to wait for before waking.
const BATCH_SIZE: usize = 4;

/// Size of the `io_uring_recvmsg_out` header the kernel writes at the start of each provided buffer
const RECVMSG_OUT_HDR: usize = 16;

/// Space reserved for control messages (cmsg) in each provided buffer.
const CMSG_BUF_SIZE: usize = 128;

/// Total size of each provided buffer entry.
///
/// Layout: `[recvmsg_out header][control data][CAN frame payload]`
const BUF_ENTRY_SIZE: usize = RECVMSG_OUT_HDR + CMSG_BUF_SIZE + FRAME_SIZE;

/// Receives CAN frames from multiple interfaces using io_uring multishot RecvMsgMulti and sends
/// them to the writer thread through a crossbeam channel.
pub struct Receiver {
    ring: IoUring,
    sockets: Vec<OwnedFd>,
    framebuf_ring_ptr: *mut u8,
    framebuf_ring_layout: Layout,
    framebuf_data: Box<[u8]>,
}

// SAFETY: `framebuf_ring_ptr` points to memory we exclusively own, accessed only from `run()`.
unsafe impl Send for Receiver {}

impl Receiver {
    /// Create a new receiver from non-blocking sockets (from
    /// [open_can_raw](crate::can::open_can_raw)).
    ///
    /// Enables hardware timestamping and drop count reporting on each socket.
    pub fn new(sockets: Vec<OwnedFd>) -> std::io::Result<Self> {
        for sock in &sockets {
            can::enable_timestamps(sock.as_fd())?;
            can::enable_drop_count(sock.as_fd())?;
        }

        let sq_size = (sockets.len() as u32).next_power_of_two().max(4);
        let ring = IoUring::builder()
            .setup_single_issuer()
            .setup_coop_taskrun()
            .setup_cqsize(FRAMEBUF_COUNT as u32)
            .build(sq_size)?;

        let raw_fds: Vec<RawFd> = sockets.iter().map(|s| s.as_raw_fd()).collect();
        ring.submitter().register_files(&raw_fds)?;

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

        for i in 0..FRAMEBUF_COUNT as usize {
            unsafe {
                let entry = &mut *framebuf_base.add(i);
                entry.set_addr(framebuf_data.as_ptr().add(i * BUF_ENTRY_SIZE) as u64);
                entry.set_len(BUF_ENTRY_SIZE as u32);
                entry.set_bid(i as u16);
            }
        }

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

    /// Run the receive loop until `stop` is set.
    ///
    /// Each CQE drain produces a Vec of [CanFrame] which is sent through `full_tx`.
    /// Empty Vecs are pulled from `empty_rx` and reused, falling back to a fresh allocation
    /// when the recycle channel is empty. If `full_tx` is disconnected (writer thread dropped),
    /// the receiver treats it as a shutdown signal. Returns the total number of frames received.
    pub fn run(
        &mut self,
        stop: &AtomicBool,
        full_tx: &crossbeam_channel::Sender<Vec<CanFrame>>,
        empty_rx: &crossbeam_channel::Receiver<Vec<CanFrame>>,
    ) -> std::io::Result<u64> {
        let mut total = 0u64;
        // We wait for BATCH_SIZE completions before waking up, but we still want to be able to
        // react to the stop signal in a timely manner, so we set a 100ms timeout.
        let timeout = types::Timespec::new().nsec(100_000_000);
        let args = types::SubmitArgs::new().timespec(&timeout);
        let framebuf_base = self.framebuf_ring_ptr as *mut BufRingEntry;
        let mask = FRAMEBUF_COUNT - 1;

        let mut pending_resubmit: Vec<usize> = Vec::new();

        let mut msghdr: libc::msghdr = unsafe { std::mem::zeroed() };
        msghdr.msg_controllen = CMSG_BUF_SIZE;

        for idx in 0..self.sockets.len() {
            let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                .build()
                .user_data(idx as u64);
            unsafe { self.ring.submission().push(&entry) }
                .map_err(|_| std::io::Error::other("SQ full"))?;
        }

        while !stop.load(Ordering::Relaxed) {
            match self.ring.submitter().submit_with_args(BATCH_SIZE, &args) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::ETIME) => {}
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => return Err(e),
            }

            pending_resubmit.retain(|&idx| {
                let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                    .build()
                    .user_data(idx as u64);
                unsafe { self.ring.submission().push(&entry) }.is_err()
            });

            let mut cqe_buf = [(0u64, 0i32, 0u32); FRAMEBUF_COUNT as usize];
            let mut cqe_count = 0;
            for cqe in self.ring.completion() {
                cqe_buf[cqe_count] = (cqe.user_data(), cqe.result(), cqe.flags());
                cqe_count += 1;
            }

            let mut batch = empty_rx
                .try_recv()
                .unwrap_or_else(|_| Vec::with_capacity(BATCH_CAPACITY));

            for &(ud, result, flags) in &cqe_buf[..cqe_count] {
                let idx = ud as usize;

                if result < 0 {
                    let err_code = -result;
                    if err_code != libc::ECANCELED && err_code != libc::ENOBUFS {
                        return Err(std::io::Error::from_raw_os_error(err_code));
                    }
                } else if let Some(buf_id) = cqueue::buffer_select(flags) {
                    let buf_offset = buf_id as usize * BUF_ENTRY_SIZE;
                    let buf = &self.framebuf_data[buf_offset..buf_offset + BUF_ENTRY_SIZE];

                    if let Ok(out) = types::RecvMsgOut::parse(buf, &msghdr) {
                        let payload = out.payload_data();
                        if payload.len() == FRAME_SIZE {
                            let raw = unsafe {
                                std::ptr::read_unaligned(payload.as_ptr().cast::<LinuxCanFrame>())
                            };
                            let meta = parse_control_data(out.control_data());
                            let timestamp = meta.timestamp.unwrap_or(Timestamp { sec: 0, nsec: 0 });

                            batch.push(CanFrame {
                                sock_id: idx,
                                timestamp,
                                direction: Direction::Rx,
                                raw,
                            });
                            total += 1;
                        }
                    }

                    self.return_buffer(framebuf_base, buf_id, mask);
                }

                if !cqueue::more(flags) {
                    let entry = opcode::RecvMsgMulti::new(types::Fixed(idx as u32), &msghdr, BGID)
                        .build()
                        .user_data(ud);
                    if unsafe { self.ring.submission().push(&entry) }.is_err() {
                        pending_resubmit.push(idx);
                    }
                }
            }

            if !batch.is_empty() && full_tx.send(batch).is_err() {
                // Channel disconnected; writer thread is gone.
                return Ok(total);
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

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.ring.submitter().unregister_buf_ring(BGID);
        unsafe {
            std::alloc::dealloc(self.framebuf_ring_ptr, self.framebuf_ring_layout);
        }
    }
}

/// Metadata extracted from recvmsg control data.
struct FrameMeta {
    timestamp: Option<Timestamp>,
}

/// Parse the cmsg chain from recvmsg control data.
///
/// Walks the control buffer looking for `SCM_TIMESTAMPING` (3 timespecs: software, deprecated,
/// hw_raw). Prefers hw_raw (index 2), falls back to software (index 0).
fn parse_control_data(control: &[u8]) -> FrameMeta {
    let mut meta = FrameMeta { timestamp: None };
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

        if hdr.cmsg_level == libc::SOL_SOCKET
            && hdr.cmsg_type == libc::SCM_TIMESTAMPING
            && data_start + data_len <= control.len()
        {
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

        offset += (cmsg_len + align - 1) & !(align - 1);
    }

    meta
}

#[cfg(test)]
mod tests {
    use std::os::unix::io::AsFd;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Receiver;
    use crate::can::{self, LinuxCanFrame};
    use crate::frame::Direction;

    const IFACE_COUNT: usize = 4;
    const FRAMES_PER_IFACE: usize = 100;
    const TOTAL_FRAMES: u64 = (IFACE_COUNT * FRAMES_PER_IFACE) as u64;

    #[test]
    #[cfg_attr(feature = "ci", ignore = "requires vcan")]
    fn recv_frames_from_multiple_interfaces() {
        let vcans = vcan_fixture::VcanHarness::new(IFACE_COUNT).unwrap();

        // Open RX sockets first so they see the frames we send.
        let mut rx_sockets = Vec::with_capacity(IFACE_COUNT);
        for name in vcans.names() {
            rx_sockets.push(can::open_can_raw(name).unwrap());
        }

        // Send test frames on separate TX sockets.
        for (iface_idx, name) in vcans.names().iter().enumerate() {
            let tx = can::open_can_raw_blocking(name).unwrap();
            for frame_idx in 0..FRAMES_PER_IFACE {
                let frame = LinuxCanFrame::new(
                    ((iface_idx as u32) << 8) | (frame_idx as u32) | libc::CAN_EFF_FLAG,
                    &[iface_idx as u8, frame_idx as u8],
                );
                can::send_frame(tx.as_fd(), &frame).unwrap();
            }
        }

        static STOP: AtomicBool = AtomicBool::new(false);
        STOP.store(false, Ordering::Relaxed);
        let (full_tx, full_rx) = crossbeam_channel::unbounded::<Vec<_>>();
        let (empty_tx, empty_rx) = crossbeam_channel::bounded::<Vec<_>>(8);
        for _ in 0..4 {
            empty_tx
                .send(Vec::with_capacity(super::BATCH_CAPACITY))
                .unwrap();
        }

        // Receiver must be created on the same thread that calls run() due to SINGLE_ISSUER.
        let handle = std::thread::spawn(move || {
            let mut recv = Receiver::new(rx_sockets).unwrap();
            recv.run(&STOP, &full_tx, &empty_rx)
        });

        let mut count = 0u64;
        while count < TOTAL_FRAMES {
            let mut batch = full_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .unwrap();
            for frame in &batch {
                assert!(frame.sock_id < IFACE_COUNT);
                assert_eq!(frame.direction, Direction::Rx);
                assert!(frame.raw.len <= 8);
                count += 1;
            }
            batch.clear();
            let _ = empty_tx.try_send(batch);
        }

        STOP.store(true, Ordering::Relaxed);
        let total = handle.join().unwrap().unwrap();
        assert_eq!(total, TOTAL_FRAMES);
    }
}
