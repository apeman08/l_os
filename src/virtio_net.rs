//! virtio-net driver (legacy "transitional" device, I/O-port transport).
//!
//! * Nothing here runs until `bring_up` is called, and the kernel only calls
//!   it once a connection has been authorized: until then the device stays in
//!   its reset state and never sends or receives a frame.
//! * DMA buffers and rings come from the kernel's frame allocator (zeroed,
//!   physically known). There is no IOMMU: a malicious or buggy *device* could
//!   still read or write any RAM. That trust in the (emulated) hardware is
//!   documented in docs/THREAT_MODEL.md.
//! * Packet handling is polled; the device interrupt only wakes processes that
//!   are waiting for the network.

use crate::{memory, pci};
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering, fence};
use smoltcp::{
    phy::{self, Device, DeviceCapabilities, Medium},
    time::Instant,
};
use x86_64::{
    VirtAddr,
    instructions::port::Port,
    structures::paging::{FrameAllocator, PhysFrame},
};

pub const VIRTIO_VENDOR: u16 = 0x1af4;
pub const VIRTIO_NET_LEGACY: u16 = 0x1000;

const REG_HOST_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0c;
const REG_QUEUE_SELECT: u16 = 0x0e;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_ISR: u16 = 0x13;
const REG_MAC: u16 = 0x14;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 0x80;
const FEATURE_MAC: u32 = 1 << 5;

const HDR_LEN: usize = 10; // struct virtio_net_hdr without mergeable buffers
const BUF_SIZE: usize = 2048;
const RX_BUFS: usize = 32;
const TX_BUFS: usize = 16;
const MTU: usize = 1514;

const DESC_F_WRITE: u16 = 2;

/// I/O base of the active NIC (0 = none), readable from the interrupt handler.
static ACTIVE_IO: AtomicU16 = AtomicU16::new(0);
static TX_FRAMES: AtomicU64 = AtomicU64::new(0);
static RX_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Frames put on the wire / taken from it since boot.
pub fn counters() -> (u64, u64) {
    (TX_FRAMES.load(Ordering::Relaxed), RX_FRAMES.load(Ordering::Relaxed))
}

/// Interrupt handler body: acknowledge the device (reading ISR clears it).
/// Returns whether a NIC is active.
pub fn acknowledge_irq() -> bool {
    let io = ACTIVE_IO.load(Ordering::Relaxed);
    if io == 0 {
        return false;
    }
    // SAFETY: `io` is the I/O base of the virtio device that bring_up claimed.
    unsafe { Port::<u8>::new(io + REG_ISR).read() };
    true
}

fn inb(io: u16, reg: u16) -> u8 {
    // SAFETY: I/O port of the claimed virtio device (see callers).
    unsafe { Port::<u8>::new(io + reg).read() }
}
fn inw(io: u16, reg: u16) -> u16 {
    // SAFETY: as above.
    unsafe { Port::<u16>::new(io + reg).read() }
}
fn inl(io: u16, reg: u16) -> u32 {
    // SAFETY: as above.
    unsafe { Port::<u32>::new(io + reg).read() }
}
fn outb(io: u16, reg: u16, v: u8) {
    // SAFETY: as above.
    unsafe { Port::<u8>::new(io + reg).write(v) }
}
fn outw(io: u16, reg: u16, v: u16) {
    // SAFETY: as above.
    unsafe { Port::<u16>::new(io + reg).write(v) }
}
fn outl(io: u16, reg: u16, v: u32) {
    // SAFETY: as above.
    unsafe { Port::<u32>::new(io + reg).write(v) }
}

/// What the PCI scan found, readable without enabling anything.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub pci: pci::Device,
    pub io: u16,
    pub mac: [u8; 6],
}

/// Look for a virtio-net device and read its MAC from configuration space.
/// This does not initialize the device or send anything.
pub fn probe() -> Option<Probe> {
    let dev = pci::find(VIRTIO_VENDOR, VIRTIO_NET_LEGACY)?;
    if dev.bars[0] & 1 == 0 {
        return None; // not an I/O BAR
    }
    let io = (dev.bars[0] & !3) as u16;
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = inb(io, REG_MAC + i as u16);
    }
    Some(Probe { pci: dev, io, mac })
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// One split virtqueue in physically contiguous, zeroed memory.
struct Virtq {
    size: u16,
    virt: u64,
    used_off: u64,
    avail_idx: u16,
    last_used: u16,
}

impl Virtq {
    fn avail(&self) -> u64 {
        self.virt + 16 * self.size as u64
    }
    fn used(&self) -> u64 {
        self.virt + self.used_off
    }

    fn set_desc(&mut self, i: u16, addr: u64, len: u32, flags: u16) {
        let p = (self.virt + 16 * i as u64) as *mut Desc;
        // SAFETY: `i < size`; the descriptor table lives in our queue memory.
        unsafe { p.write_volatile(Desc { addr, len, flags, next: 0 }) };
    }

    /// Make descriptor `id` available to the device.
    fn push_avail(&mut self, id: u16) {
        let slot = self.avail_idx % self.size;
        // SAFETY: ring[slot] and idx are inside the avail area of our queue.
        unsafe {
            ((self.avail() + 4 + 2 * slot as u64) as *mut u16).write_volatile(id);
            fence(Ordering::SeqCst);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            ((self.avail() + 2) as *mut u16).write_volatile(self.avail_idx);
        }
        fence(Ordering::SeqCst);
    }

    fn used_idx(&self) -> u16 {
        // SAFETY: the used.idx field is inside our queue memory.
        unsafe { ((self.used() + 2) as *const u16).read_volatile() }
    }

    /// Next completed buffer `(id, len)`, if any (does not consume it).
    fn peek_used(&self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        if self.used_idx() == self.last_used {
            return None;
        }
        let slot = (self.last_used % self.size) as u64;
        // SAFETY: used ring entry `slot` lies in our queue memory.
        let (id, len) = unsafe {
            let e = (self.used() + 4 + 8 * slot) as *const u32;
            (e.read_volatile(), e.add(1).read_volatile())
        };
        Some((id as u16, len))
    }

    fn pop_used(&mut self) {
        self.last_used = self.last_used.wrapping_add(1);
    }
}

fn ring_layout(size: u16) -> (u64, usize) {
    let size = size as u64;
    let first = 16 * size + 6 + 2 * size;
    let used_off = first.div_ceil(4096) * 4096;
    let total = used_off + 6 + 8 * size;
    (used_off, total.div_ceil(4096) as usize)
}

struct RxSide {
    q: Virtq,
    bufs: [u64; RX_BUFS], // physical addresses
}

struct TxSide {
    q: Virtq,
    bufs: [u64; TX_BUFS],
    busy: u32,
}

pub struct VirtioNet {
    io: u16,
    pub mac: [u8; 6],
    rx: RxSide,
    tx: TxSide,
}

#[derive(Debug)]
pub enum BringUpError {
    NoMemory,
    BadQueue,
}

fn setup_queue(io: u16, index: u16, mem: &mut memory::Memory) -> Result<Virtq, BringUpError> {
    outw(io, REG_QUEUE_SELECT, index);
    let size = inw(io, REG_QUEUE_SIZE);
    if size == 0 || !size.is_power_of_two() || size > 1024 {
        return Err(BringUpError::BadQueue);
    }
    let (used_off, pages) = ring_layout(size);
    let frame = mem.frames.allocate_contiguous(pages).ok_or(BringUpError::NoMemory)?;
    outl(io, REG_QUEUE_PFN, (frame.start_address().as_u64() / 4096) as u32);
    Ok(Virtq {
        size,
        virt: (memory::phys_offset() + frame.start_address().as_u64()).as_u64(),
        used_off,
        avail_idx: 0,
        last_used: 0,
    })
}

fn alloc_buf(mem: &mut memory::Memory) -> Result<u64, BringUpError> {
    let f: PhysFrame = mem.frames.allocate_frame().ok_or(BringUpError::NoMemory)?;
    Ok(f.start_address().as_u64())
}

fn virt_of(phys: u64) -> *mut u8 {
    (memory::phys_offset() + phys).as_mut_ptr::<u8>()
}

impl VirtioNet {
    /// Initialize the device and its queues. This is the point where the NIC
    /// becomes active; the caller must have decided that is authorized.
    pub fn bring_up(p: &Probe) -> Result<VirtioNet, BringUpError> {
        let io = p.io;
        pci::enable_bus_mastering(&p.pci);

        outb(io, REG_STATUS, 0); // reset
        outb(io, REG_STATUS, STATUS_ACK);
        outb(io, REG_STATUS, STATUS_ACK | STATUS_DRIVER);
        let host = inl(io, REG_HOST_FEATURES);
        outl(io, REG_GUEST_FEATURES, host & FEATURE_MAC); // MAC only: no offloads

        let mut guard = memory::MEMORY.lock();
        let mem = guard.as_mut().ok_or(BringUpError::NoMemory)?;
        let result = (|| {
            let mut rxq = setup_queue(io, 0, mem)?;
            let mut txq = setup_queue(io, 1, mem)?;
            let mut rx_bufs = [0u64; RX_BUFS];
            for (i, b) in rx_bufs.iter_mut().enumerate() {
                *b = alloc_buf(mem)?;
                rxq.set_desc(i as u16, *b, BUF_SIZE as u32, DESC_F_WRITE);
                rxq.push_avail(i as u16);
            }
            let mut tx_bufs = [0u64; TX_BUFS];
            for b in tx_bufs.iter_mut() {
                *b = alloc_buf(mem)?;
            }
            Ok((rxq, txq, rx_bufs, tx_bufs))
        })();
        let (rxq, txq, rx_bufs, tx_bufs) = match result {
            Ok(v) => v,
            Err(e) => {
                outb(io, REG_STATUS, STATUS_FAILED);
                return Err(e);
            }
        };
        drop(guard);

        outb(io, REG_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
        outw(io, REG_QUEUE_NOTIFY, 0); // tell the device RX buffers are ready
        ACTIVE_IO.store(io, Ordering::SeqCst);
        Ok(VirtioNet {
            io,
            mac: p.mac,
            rx: RxSide { q: rxq, bufs: rx_bufs },
            tx: TxSide { q: txq, bufs: tx_bufs, busy: 0 },
        })
    }

    fn reclaim_tx(&mut self) {
        while let Some((id, _)) = self.tx.q.peek_used() {
            if (id as usize) < TX_BUFS {
                self.tx.busy &= !(1 << id);
            }
            self.tx.q.pop_used();
        }
    }
}

pub struct RxToken<'a> {
    rx: &'a mut RxSide,
    io: u16,
    id: u16,
    len: usize,
}

pub struct TxToken<'a> {
    tx: &'a mut TxSide,
    io: u16,
    idx: usize,
}

impl phy::RxToken for RxToken<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        let buf = virt_of(self.rx.bufs[self.id as usize]);
        let frame_len = self.len.saturating_sub(HDR_LEN).min(BUF_SIZE - HDR_LEN);
        // SAFETY: the device finished writing this buffer (it is in the used
        // ring) and it is exclusively ours until re-posted below; the slice
        // stays within the buffer.
        let r = f(unsafe { core::slice::from_raw_parts(buf.add(HDR_LEN), frame_len) });
        self.rx.q.pop_used();
        RX_FRAMES.fetch_add(1, Ordering::Relaxed);
        self.rx.q.push_avail(self.id);
        outw(self.io, REG_QUEUE_NOTIFY, 0);
        r
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let len = len.min(MTU);
        let phys = self.tx.bufs[self.idx];
        let buf = virt_of(phys);
        // SAFETY: the buffer is ours (not busy) and HDR_LEN + MTU < BUF_SIZE.
        let r = unsafe {
            core::ptr::write_bytes(buf, 0, HDR_LEN); // no offloads: zero header
            f(core::slice::from_raw_parts_mut(buf.add(HDR_LEN), len))
        };
        self.tx.busy |= 1 << self.idx;
        self.tx.q.set_desc(self.idx as u16, phys, (HDR_LEN + len) as u32, 0);
        self.tx.q.push_avail(self.idx as u16);
        outw(self.io, REG_QUEUE_NOTIFY, 1);
        TX_FRAMES.fetch_add(1, Ordering::Relaxed);
        r
    }
}

impl Device for VirtioNet {
    type RxToken<'a> = RxToken<'a>;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _ts: Instant) -> Option<(RxToken<'_>, TxToken<'_>)> {
        self.reclaim_tx();
        let idx = (0..TX_BUFS).find(|i| self.tx.busy & (1 << i) == 0)?;
        let (id, len) = self.rx.q.peek_used()?;
        if id as usize >= RX_BUFS {
            self.rx.q.pop_used(); // malformed completion: drop it
            return None;
        }
        Some((
            RxToken { rx: &mut self.rx, io: self.io, id, len: len as usize },
            TxToken { tx: &mut self.tx, io: self.io, idx },
        ))
    }

    fn transmit(&mut self, _ts: Instant) -> Option<TxToken<'_>> {
        self.reclaim_tx();
        let idx = (0..TX_BUFS).find(|i| self.tx.busy & (1 << i) == 0)?;
        Some(TxToken { tx: &mut self.tx, io: self.io, idx })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(1);
        caps
    }
}

