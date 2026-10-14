//! xHCI host controller: reset, rings and command submission (spec §7.1
//! steps 1–4, xHCI 1.2 section 4.2).
//!
//! One controller (the first PCI function with class 0x0C/0x03/0x30), one
//! command ring and one event ring of one segment each, interrupter 0 with
//! interrupts off: the driver polls. [`command`] is for the boot phase,
//! before the timer-ISR polling of Task 20 exists; it holds [`DRIVER`]'s lock
//! while it waits.
//!
//! **Hardware is untrusted.** Every register value and every event TRB is
//! only compared or decoded (never used as an address), every wait is
//! bounded by ticks and the TSC ([`Deadline`]), and a controller that reads
//! all-ones (`USBSTS == 0xFFFF_FFFF`, gone from the bus) is
//! [`XhciError::ControllerGone`], never a hang or a panic.
//!
//! **DMA memory** (DCBAA, scratchpad array and buffers, command ring, event
//! ring, ERST) comes zeroed from [`memory::BootInfoFrameAllocator::allocate_contiguous`]:
//! page aligned (so 64-byte aligned), physically contiguous per structure,
//! below 4 GiB (the allocator never hands out higher frames; checked against
//! `AC64` anyway). The frames stay allocated for the kernel's lifetime (a
//! re-init reuses and re-zeroes them), so wipe stage 7 (`AllocatedZeroed`)
//! zeroes them; stage 3 (`DmaStopped`) halts the controller first through
//! [`halt_for_wipe`] and then clears its bus-master bit.
//!
//! **Ordering:** DMA memory is ordinary write-back RAM (x86 DMA is cache
//! coherent); registers go through the uncached window ([`crate::mmio`]).
//! A producer TRB is written body first, then its control dword with the
//! cycle bit, then the link TRB (if the ring wraps), then a full fence and
//! the doorbell (ledger Task 12). An event TRB is read control dword first
//! (the controller writes the cycle bit last), then the body.

use crate::{memory, mmio, pci, time};
use core::sync::atomic::{AtomicU64, Ordering, fence};
use l_core::xhci_ctx::Caps;
use l_core::xhci_ring::{self as ring, Event, EventRing, ProducerRing, Trb};
use spin::Mutex;

// Operational registers, offsets from CAPLENGTH (section 5.4).
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const PAGESIZE: u64 = 0x08;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
/// First port register set (PORTSC of port 1), section 5.4.8.
const PORT_REGS: u64 = 0x400;
// Interrupter 0, offsets from RTSOFF (section 5.5.2).
const IR0: u64 = 0x20;
const IMAN: u64 = 0x00;
const IMOD: u64 = 0x04;
const ERSTSZ: u64 = 0x08;
const ERSTBA: u64 = 0x10;
const ERDP: u64 = 0x18;

const CMD_RS: u32 = 1;
const CMD_HCRST: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2;
const STS_HCH: u32 = 1;
const STS_HSE: u32 = 1 << 2;
const STS_CNR: u32 = 1 << 11;
const STS_HCE: u32 = 1 << 12;
/// What a register reads when the device is gone from the bus.
const ALL_ONES: u32 = 0xFFFF_FFFF;
/// CRCR Ring Cycle State.
const CRCR_RCS: u64 = 1;
/// ERDP Event Handler Busy (RW1C: writing 1 clears it).
const ERDP_EHB: u64 = 1 << 3;
/// IMAN Interrupt Pending (RW1C); IE (bit 1) stays 0.
const IMAN_IP: u32 = 1;

/// TRBs in the command ring, including the link TRB (spec §7.1 step 3).
pub const COMMAND_TRBS: usize = 64;
/// TRBs in the single event ring segment.
pub const EVENT_TRBS: usize = 64;
/// Non-command events kept for the port/transfer handling of Task 20.
pub const EVENT_QUEUE_LEN: usize = 32;
/// Default bound of every wait in [`init`] (spec §7.1 step 2).
pub const DEFAULT_TIMEOUT_MS: u64 = 500;
/// Bound of the halt wait in [`halt_for_wipe`]; xHCI 1.2 section 5.4.1
/// requires HCH within 16 ms of `RS = 0`.
const HALT_WAIT_MS: u64 = 20;
/// TSC cycles per millisecond assumed while the TSC is not calibrated
/// (`time::tsc_hz() == 0`): at least 1 ms up to 5 GHz.
const FALLBACK_CYCLES_PER_MS: u64 = 5_000_000;
const PAGE: u64 = 4096;
const FOUR_GIB: u64 = 1 << 32;
/// Command TRB types (xHCI 1.2 table 6-91): Enable Slot (9) .. No Op (23).
const COMMAND_TYPES: core::ops::RangeInclusive<u8> = ring::ENABLE_SLOT..=ring::NOOP_CMD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciError {
    /// No PCI function with class 0x0C/0x03/0x30.
    NotFound,
    /// BAR0 is not a memory BAR or has no size.
    BadBar,
    /// BAR0 could not be mapped into the MMIO window.
    Map(mmio::MapError),
    /// Capability registers describe registers outside BAR0 or zero slots.
    BadRegisters,
    /// The controller does not support 4 KiB pages (PAGESIZE bit 0).
    Unsupported,
    /// No (suitable) frames for the DMA structures.
    NoMemory,
    /// A bounded wait ran out.
    Timeout,
    /// Registers read all-ones: the controller is gone from the bus.
    ControllerGone,
    /// `USBSTS.HSE` or `USBSTS.HCE` is set (raw USBSTS).
    HostError(u32),
    /// The No-Op of init's self-test completed with this code.
    SelfTest(u8),
    /// [`init`] while the controller runs; nothing was touched.
    AlreadyInitialized,
    /// [`init`] has not completed.
    NotInitialized,
    /// The controller is halted (e.g. by [`halt_for_wipe`]); call [`init`].
    Halted,
    /// An earlier command timed out and has not completed yet; nothing new
    /// is enqueued (at most one command is in flight).
    Busy,
    /// Not a command TRB (type outside 9..=23, e.g. a link TRB).
    InvalidTrb,
}

impl From<mmio::OutOfBounds> for XhciError {
    /// Offsets are validated against BAR0 in `probe`; an out-of-bounds
    /// access therefore means the capability values were inconsistent.
    fn from(_: mmio::OutOfBounds) -> Self {
        XhciError::BadRegisters
    }
}

/// Where the command ring lives and where the next command goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRing {
    /// Physical address of TRB 0.
    pub base: u64,
    /// Usable slots (the last of the [`COMMAND_TRBS`] is the link TRB).
    pub slots: usize,
    /// Physical address the next command TRB is written to.
    pub enqueue: u64,
}

/// Wait bound: `ms` milliseconds by the tick counter *and* by the TSC, so it
/// ends even with interrupts off (ticks frozen) or an uncalibrated TSC.
struct Deadline {
    tick0: u64,
    ticks: u64,
    tsc0: u64,
    cycles: u64,
}

impl Deadline {
    fn new(ms: u64) -> Deadline {
        Deadline {
            tick0: time::ticks(),
            // +1: the current tick period is already partly over.
            ticks: time::ticks_from_ms(ms).saturating_add(1),
            tsc0: time::rdtsc(),
            cycles: cycles_for_ms(ms),
        }
    }

    fn expired(&self) -> bool {
        time::ticks().wrapping_sub(self.tick0) > self.ticks || time::rdtsc().wrapping_sub(self.tsc0) > self.cycles
    }
}

/// TSC cycles in `ms` milliseconds (lock-free: atomics only).
fn cycles_for_ms(ms: u64) -> u64 {
    let hz = time::tsc_hz();
    let per_ms = if hz == 0 { FALLBACK_CYCLES_PER_MS } else { (hz / 1000).max(1) };
    per_ms.saturating_mul(ms)
}

/// The DMA structures (physical addresses; each one contiguous run).
#[derive(Debug, Clone, Copy)]
struct Dma {
    dcbaa: u64,
    /// Scratchpad array and its frame count (0 if no scratchpad).
    scratch_array: u64,
    scratch_array_frames: usize,
    /// `n` scratchpad buffers, one page each, contiguous.
    scratch_bufs: u64,
    scratch_count: usize,
    cmd: u64,
    event: u64,
    erst: u64,
}

impl Dma {
    fn runs(&self) -> [(u64, usize); 6] {
        [
            (self.dcbaa, 1),
            (self.scratch_array, self.scratch_array_frames),
            (self.scratch_bufs, self.scratch_count),
            (self.cmd, 1),
            (self.event, 1),
            (self.erst, 1),
        ]
    }
}

/// Fixed queue of non-command events (oldest first); overflow is counted.
struct EventQueue {
    buf: [Event; EVENT_QUEUE_LEN],
    head: usize,
    len: usize,
    dropped: u64,
}

impl EventQueue {
    const fn new() -> EventQueue {
        EventQueue { buf: [Event::Other(0); EVENT_QUEUE_LEN], head: 0, len: 0, dropped: 0 }
    }

    fn push(&mut self, e: Event) {
        if self.len == EVENT_QUEUE_LEN {
            // Keep the oldest: a later port event re-reads PORTSC anyway.
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.buf[(self.head + self.len) % EVENT_QUEUE_LEN] = e;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        let e = self.buf[self.head];
        self.head = (self.head + 1) % EVENT_QUEUE_LEN;
        self.len -= 1;
        Some(e)
    }
}

struct Driver {
    regs: mmio::Mmio,
    caps: Caps,
    /// Offsets of the operational registers, interrupter 0 and doorbell 0
    /// inside `regs` (validated in `probe`).
    op: u64,
    ir0: u64,
    db: u64,
    dma: Option<Dma>,
    /// `start` completed (the controller may since have been halted by
    /// [`halt_for_wipe`]; the hardware's HCH tells).
    started: bool,
    cmd: ProducerRing,
    ev: EventRing,
    /// Physical address of a command that timed out and has not completed.
    stale: Option<u64>,
    queue: EventQueue,
}

static DRIVER: Mutex<Option<Driver>> = Mutex::new(None);

/// Lock-free copy of the operational registers' virtual address (0 = no
/// controller mapped), for [`halt_for_wipe`], which must not take
/// [`DRIVER`] (spec E14, like the PCI/ACPI mirrors of Task 10). Published
/// once in `probe`, after the mapping exists and the offsets were checked;
/// the window's mappings are never removed.
static OP_VIRT: AtomicU64 = AtomicU64::new(0);

/// Volatile 32-bit read of a register through the lock-free mirror.
///
/// # Safety
/// `op` must be the value published in [`OP_VIRT`] and `off` one of
/// `USBCMD`/`USBSTS` (checked inside the BAR mapping by `probe`).
unsafe fn op_read(op: u64, off: u64) -> u32 {
    // SAFETY: caller contract: `op + off` lies in the uncached window
    // mapping of BAR0, which is never unmapped, and is 4-byte aligned.
    unsafe { core::ptr::read_volatile((op + off) as *const u32) }
}

/// # Safety
/// As [`op_read`].
unsafe fn op_write(op: u64, off: u64, v: u32) {
    // SAFETY: as in `op_read`; the target is device memory, not a Rust
    // object.
    unsafe { core::ptr::write_volatile((op + off) as *mut u32, v) }
}

/// Raw USBSTS, lock-free; `None` before a controller was mapped.
pub fn usbsts() -> Option<u32> {
    let op = OP_VIRT.load(Ordering::Acquire);
    // SAFETY: `op` was published by `probe` (non-zero), USBSTS checked there.
    (op != 0).then(|| unsafe { op_read(op, USBSTS) })
}

/// Raw USBCMD, lock-free; `None` before a controller was mapped.
pub fn usbcmd() -> Option<u32> {
    let op = OP_VIRT.load(Ordering::Acquire);
    // SAFETY: as in `usbsts`.
    (op != 0).then(|| unsafe { op_read(op, USBCMD) })
}

/// Stop the controller for the wipe (stage 3, `DmaStopped`): clear
/// `USBCMD.RS` (and `INTE`), then wait at most [`HALT_WAIT_MS`] (TSC-bounded,
/// works with interrupts off) for `USBSTS.HCH`. Lock-free, no allocation, no
/// panic; a no-op if no controller was mapped, and returns at once if the
/// controller reads all-ones (gone). The wipe clears every bus-master bit
/// right after this, which stops DMA even if the halt times out.
pub fn halt_for_wipe() {
    let op = OP_VIRT.load(Ordering::Acquire);
    if op == 0 {
        return;
    }
    // SAFETY: `op` was published by `probe`; USBCMD/USBSTS lie in the
    // mapping (checked there). Racing a locked access of the driver only
    // changes register values that the interrupted code never uses again
    // on the wipe path; it is not a memory-safety issue.
    unsafe {
        let cmd = op_read(op, USBCMD);
        if cmd == ALL_ONES {
            return;
        }
        // HCRST reads 0 when idle; never write it back as 1.
        op_write(op, USBCMD, cmd & !(CMD_RS | CMD_INTE | CMD_HCRST));
    }
    let (t0, cycles) = (time::rdtsc(), cycles_for_ms(HALT_WAIT_MS));
    loop {
        // SAFETY: as above.
        let sts = unsafe { op_read(op, USBSTS) };
        if sts & STS_HCH != 0 || sts == ALL_ONES || time::rdtsc().wrapping_sub(t0) > cycles {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Capabilities of the controller once [`init`] has mapped it.
pub fn caps() -> Option<Caps> {
    DRIVER.lock().as_ref().map(|d| d.caps)
}

/// Command ring position, `None` until [`init`] succeeded once.
pub fn command_ring() -> Option<CommandRing> {
    let g = DRIVER.lock();
    let d = g.as_ref().filter(|d| d.started)?;
    let base = d.dma?.cmd;
    Some(CommandRing { base, slots: COMMAND_TRBS - 1, enqueue: d.cmd.enqueue_ptr() })
}

/// Oldest queued non-command event (port status change, transfer, ...)
/// that [`command`] or [`init`] saw while waiting for a completion.
pub fn pop_event() -> Option<Event> {
    DRIVER.lock().as_mut()?.queue.pop()
}

/// Events dropped because the queue was full, plus command completions that
/// matched no command (hostile or stale hardware).
pub fn dropped_events() -> u64 {
    DRIVER.lock().as_ref().map_or(0, |d| d.queue.dropped)
}

/// Find, map, reset and start the controller and run the No-Op self-test
/// (spec §7.1 steps 1–4). Rejected with [`XhciError::AlreadyInitialized`]
/// while the controller runs; after a failure or a halt it resets the
/// controller again, reusing the mapping and the DMA frames.
pub fn init() -> Result<(), XhciError> {
    let mut g = DRIVER.lock();
    if let Some(d) = g.as_ref() {
        if d.started && d.sts().is_ok_and(|s| s & STS_HCH == 0) {
            return Err(XhciError::AlreadyInitialized);
        }
    }
    let d = match &mut *g {
        Some(d) => d,
        none => none.insert(Driver::probe()?),
    };
    let r = d.start();
    if r.is_err() {
        d.started = false;
        d.halt();
    }
    r
}

/// Submit one command TRB (cycle bit is set by the ring), ring doorbell 0
/// and wait at most `timeout_ms` for its Command Completion Event, which is
/// returned whatever its completion code (the caller checks it). Events
/// for other things are queued ([`pop_event`]). Only for the boot phase:
/// holds the driver lock while waiting.
pub fn command(trb: Trb, timeout_ms: u64) -> Result<Event, XhciError> {
    let mut g = DRIVER.lock();
    match g.as_mut() {
        Some(d) if d.started => d.submit(trb, timeout_ms),
        _ => Err(XhciError::NotInitialized),
    }
}

/// Physical frames for DMA: `n` contiguous zeroed frames.
fn alloc_frames(n: usize) -> Result<u64, XhciError> {
    let mut g = memory::MEMORY.lock();
    let mem = g.as_mut().ok_or(XhciError::NoMemory)?;
    mem.frames.allocate_contiguous(n).map(|f| f.start_address().as_u64()).ok_or(XhciError::NoMemory)
}

/// Virtual address of DMA memory at physical `phys` (offset mapping).
fn dma_virt(phys: u64) -> u64 {
    (memory::phys_offset() + phys).as_u64()
}

/// Write one 32-bit word of DMA memory.
///
/// # Safety
/// `phys..phys+4` must lie in a frame of this driver's [`Dma`] (allocated
/// by `alloc_frames`, never freed), 4-byte aligned.
unsafe fn dma_write32(phys: u64, v: u32) {
    // SAFETY: caller contract; the frame is RAM mapped at the physical
    // offset and owned by the driver (no Rust reference to it exists), so a
    // volatile store aliases nothing.
    unsafe { core::ptr::write_volatile(dma_virt(phys) as *mut u32, v) }
}

/// # Safety
/// As [`dma_write32`].
unsafe fn dma_read32(phys: u64) -> u32 {
    // SAFETY: as in `dma_write32`.
    unsafe { core::ptr::read_volatile(dma_virt(phys) as *const u32) }
}

/// Write a producer TRB: body, then (after a release fence) the control
/// dword carrying the cycle bit.
///
/// # Safety
/// `phys` must be a TRB slot (16 bytes) of the driver's command ring.
unsafe fn write_trb(phys: u64, t: Trb) {
    let w = t.to_words();
    // SAFETY: caller contract: the 16 bytes at `phys` are a slot of our ring.
    unsafe {
        dma_write32(phys, w[0]);
        dma_write32(phys + 4, w[1]);
        dma_write32(phys + 8, w[2]);
        fence(Ordering::Release);
        dma_write32(phys + 12, w[3]);
    }
}

impl Driver {
    /// Step 1: find the controller, map BAR0, read and check the
    /// capabilities, enable bus mastering, publish the lock-free mirror.
    fn probe() -> Result<Driver, XhciError> {
        let dev = pci::devices()
            .find(|d| d.class == 0x0c && d.subclass == 0x03 && d.prog_if == 0x30)
            .ok_or(XhciError::NotFound)?;
        let base = pci::bar64(&dev, 0).mem_base().ok_or(XhciError::BadBar)?;
        let size = pci::bar_size(&dev, 0).ok_or(XhciError::BadBar)?;
        let regs = mmio::map(base, size).map_err(XhciError::Map)?;
        // Capability registers only with 32-bit reads (spike note §3).
        let dw0 = regs.read32(0x00)?;
        if dw0 == ALL_ONES {
            return Err(XhciError::ControllerGone);
        }
        let caps = Caps::decode(dw0, regs.read32(0x04)?, regs.read32(0x08)?, regs.read32(0x10)?, regs.read32(0x14)?, regs.read32(0x18)?);
        let op = u64::from(caps.cap_length);
        let ir0 = u64::from(caps.runtime_offset) + IR0;
        let db = u64::from(caps.doorbell_offset);
        let len = regs.len();
        let ports_end = op + PORT_REGS + 0x10 * u64::from(caps.max_ports);
        let ok = caps.cap_length >= 0x20
            && caps.cap_length % 4 == 0
            && caps.max_slots >= 1
            && ports_end <= len
            && caps.runtime_offset != 0
            && ir0 + 0x20 <= len
            && db >= op + PORT_REGS
            && db + 4 * (u64::from(caps.max_slots) + 1) <= len;
        if !ok {
            return Err(XhciError::BadRegisters);
        }
        pci::enable_bus_mastering(&dev);
        OP_VIRT.store(regs.virt() + op, Ordering::Release);
        Ok(Driver {
            regs,
            caps,
            op,
            ir0,
            db,
            dma: None,
            started: false,
            cmd: ProducerRing::new(0, COMMAND_TRBS),
            ev: EventRing::new(EVENT_TRBS),
            stale: None,
            queue: EventQueue::new(),
        })
    }

    fn read_op(&self, off: u64) -> Result<u32, XhciError> {
        Ok(self.regs.read32(self.op + off)?)
    }

    fn write_op(&self, off: u64, v: u32) -> Result<(), XhciError> {
        Ok(self.regs.write32(self.op + off, v)?)
    }

    /// USBSTS; all-ones means the controller is gone.
    fn sts(&self) -> Result<u32, XhciError> {
        match self.read_op(USBSTS)? {
            ALL_ONES => Err(XhciError::ControllerGone),
            s => Ok(s),
        }
    }

    /// Spin until `cond` holds, at most `ms` milliseconds.
    fn wait(&self, ms: u64, mut cond: impl FnMut(&Driver) -> Result<bool, XhciError>) -> Result<(), XhciError> {
        let dl = Deadline::new(ms);
        loop {
            if cond(self)? {
                return Ok(());
            }
            if dl.expired() {
                return if cond(self)? { Ok(()) } else { Err(XhciError::Timeout) };
            }
            core::hint::spin_loop();
        }
    }

    /// Best effort `RS = 0` after a failed start (bounded, errors ignored).
    fn halt(&self) {
        if let Ok(c) = self.read_op(USBCMD) {
            if c != ALL_ONES {
                let _ = self.write_op(USBCMD, c & !(CMD_RS | CMD_INTE | CMD_HCRST));
                let _ = self.wait(HALT_WAIT_MS, |d| Ok(d.sts()? & STS_HCH != 0));
            }
        }
    }

    /// Steps 2–4: halt, reset, set up the DMA structures, run, self-test.
    fn start(&mut self) -> Result<(), XhciError> {
        self.started = false;
        let t = DEFAULT_TIMEOUT_MS;
        // No operational register may be written while CNR = 1 (4.2).
        self.wait(t, |d| Ok(d.sts()? & STS_CNR == 0))?;
        if self.sts()? & STS_HCH == 0 {
            let c = self.read_op(USBCMD)?;
            self.write_op(USBCMD, c & !(CMD_RS | CMD_INTE | CMD_HCRST))?;
            self.wait(t, |d| Ok(d.sts()? & STS_HCH != 0))?;
        }
        self.write_op(USBCMD, CMD_HCRST)?;
        self.wait(t, |d| {
            let c = d.read_op(USBCMD)?;
            if c == ALL_ONES {
                return Err(XhciError::ControllerGone);
            }
            Ok(c & CMD_HCRST == 0 && d.sts()? & STS_CNR == 0)
        })?;
        if self.read_op(PAGESIZE)? & 1 == 0 {
            return Err(XhciError::Unsupported);
        }

        let dma = self.setup_dma()?;
        let caps = self.caps;
        let config = self.read_op(CONFIG)?;
        self.write_op(CONFIG, config & !0xff | u32::from(caps.max_slots))?;
        let r = &self.regs;
        r.write64(self.op + DCBAAP, dma.dcbaa)?;
        r.write64(self.op + CRCR, dma.cmd | CRCR_RCS)?;
        // Interrupter 0: no interrupts (IE = 0, pending cleared), no
        // moderation; the driver polls (spec E4).
        r.write32(self.ir0 + IMAN, IMAN_IP)?;
        r.write32(self.ir0 + IMOD, 0)?;
        let erstsz = r.read32(self.ir0 + ERSTSZ)?;
        r.write32(self.ir0 + ERSTSZ, erstsz & !0xffff | 1)?;
        r.write64(self.ir0 + ERDP, dma.event | ERDP_EHB)?;
        // ERSTBA last: writing it makes the controller read the ERST (4.9.4).
        r.write64(self.ir0 + ERSTBA, dma.erst)?;

        self.cmd = ProducerRing::new(dma.cmd, COMMAND_TRBS);
        self.ev = EventRing::new(EVENT_TRBS);
        self.stale = None;
        self.queue = EventQueue::new();
        self.dma = Some(dma);

        // All DMA structures are written before the controller may run.
        fence(Ordering::SeqCst);
        let c = self.read_op(USBCMD)?;
        self.write_op(USBCMD, c & !(CMD_INTE | CMD_HCRST) | CMD_RS)?;
        self.wait(t, |d| Ok(d.sts()? & STS_HCH == 0))?;
        self.started = true;

        // Step 4: self-test.
        match self.submit(Trb::noop_cmd(), t)? {
            Event::CommandCompletion { code: ring::SUCCESS, .. } => Ok(()),
            Event::CommandCompletion { code, .. } => Err(XhciError::SelfTest(code)),
            _ => Err(XhciError::SelfTest(0)),
        }
    }

    /// Allocate the DMA structures (first start) or re-zero them (restart,
    /// the controller is reset and halted), then fill in the pointers.
    fn setup_dma(&mut self) -> Result<Dma, XhciError> {
        let caps = self.caps;
        let dma = match self.dma {
            Some(dma) => {
                for (phys, n) in dma.runs() {
                    if n > 0 {
                        // SAFETY: `phys` starts `n` frames this driver
                        // allocated and owns; the controller is halted and
                        // reset, so it does not access them now.
                        unsafe { core::ptr::write_bytes(dma_virt(phys) as *mut u8, 0, n * PAGE as usize) };
                    }
                }
                dma
            }
            None => {
                let n = usize::from(caps.max_scratchpad);
                let array_frames = (n * 8).div_ceil(PAGE as usize);
                let dma = Dma {
                    dcbaa: alloc_frames(1)?,
                    scratch_array: if n > 0 { alloc_frames(array_frames)? } else { 0 },
                    scratch_array_frames: array_frames,
                    scratch_bufs: if n > 0 { alloc_frames(n)? } else { 0 },
                    scratch_count: n,
                    cmd: alloc_frames(1)?,
                    event: alloc_frames(1)?,
                    erst: alloc_frames(1)?,
                };
                // Remember them at once: a failure below reuses, never leaks
                // more frames on the next try.
                self.dma = Some(dma);
                dma
            }
        };
        let above_4g = dma.runs().iter().any(|&(p, n)| n > 0 && p + n as u64 * PAGE > FOUR_GIB);
        if above_4g && !caps.ac64 {
            return Err(XhciError::NoMemory);
        }
        // SAFETY: every address below lies in the frames of `dma` (sizes
        // computed above: DCBAA entry 0 in its frame, `n` 8-byte entries in
        // the scratchpad array, the ERST entry in its frame), 4-byte aligned.
        unsafe {
            if dma.scratch_count > 0 {
                for i in 0..dma.scratch_count as u64 {
                    let buf = dma.scratch_bufs + i * PAGE;
                    dma_write32(dma.scratch_array + i * 8, buf as u32);
                    dma_write32(dma.scratch_array + i * 8 + 4, (buf >> 32) as u32);
                }
                dma_write32(dma.dcbaa, dma.scratch_array as u32);
                dma_write32(dma.dcbaa + 4, (dma.scratch_array >> 32) as u32);
            }
            for (i, w) in ring::erst_entry(dma.event, EVENT_TRBS as u16).into_iter().enumerate() {
                dma_write32(dma.erst + 4 * i as u64, w);
            }
        }
        Ok(dma)
    }

    /// Consume one event TRB, if the controller produced one, and move
    /// ERDP past it (EHB written as 1 to clear it).
    fn next_event(&mut self, dma: &Dma) -> Result<Option<Event>, XhciError> {
        let base = dma.event;
        let t = self.ev.poll(|i| {
            let p = base + (i as u64 % EVENT_TRBS as u64) * 16;
            // SAFETY: `p` is one of the EVENT_TRBS slots of our event ring.
            // Control dword (cycle bit) first: the controller writes it
            // last, so a matching cycle bit means the body is complete.
            unsafe {
                let c = dma_read32(p + 12);
                fence(Ordering::Acquire);
                [dma_read32(p), dma_read32(p + 4), dma_read32(p + 8), c]
            }
        });
        let Some(t) = t else { return Ok(None) };
        self.regs.write64(self.ir0 + ERDP, self.ev.erdp(base))?;
        Ok(Some(Event::decode(t)))
    }

    /// Hardware state that makes waiting pointless.
    fn check_running(&self) -> Result<(), XhciError> {
        let s = self.sts()?;
        if s & (STS_HSE | STS_HCE) != 0 {
            return Err(XhciError::HostError(s));
        }
        if s & STS_HCH != 0 {
            return Err(XhciError::Halted);
        }
        Ok(())
    }

    /// Route one event: `Some(event)` if it completes the command at
    /// `want`, otherwise it is queued (or, a completion of nothing we
    /// know, counted as dropped).
    fn route(&mut self, e: Event, want: Option<u64>) -> Option<Event> {
        match e {
            Event::CommandCompletion { trb, .. } if Some(trb) == want => return Some(e),
            Event::CommandCompletion { trb, .. } if Some(trb) == self.stale => self.stale = None,
            Event::CommandCompletion { .. } => self.queue.dropped = self.queue.dropped.saturating_add(1),
            other => self.queue.push(other),
        }
        None
    }

    fn submit(&mut self, trb: Trb, timeout_ms: u64) -> Result<Event, XhciError> {
        if !COMMAND_TYPES.contains(&trb.trb_type()) {
            return Err(XhciError::InvalidTrb);
        }
        let dma = self.dma.ok_or(XhciError::NotInitialized)?;
        self.check_running()?;
        // At most one command in flight (the ring has no full check).
        if self.stale.is_some() {
            while let Some(e) = self.next_event(&dma)? {
                self.route(e, None);
            }
            if self.stale.is_some() {
                return Err(XhciError::Busy);
            }
        }

        let enq = self.cmd.enqueue(trb);
        let at = dma.cmd + enq.index as u64 * 16;
        // SAFETY: `enq.index` and the link index are < COMMAND_TRBS, slots of
        // our command ring.
        unsafe {
            write_trb(at, enq.trb);
            if let Some((i, link)) = enq.link {
                write_trb(dma.cmd + i as u64 * 16, link);
            }
        }
        // TRBs visible before the doorbell write reaches the controller.
        fence(Ordering::SeqCst);
        self.regs.write32(self.db, 0)?;

        let dl = Deadline::new(timeout_ms);
        loop {
            while let Some(e) = self.next_event(&dma)? {
                if let Some(done) = self.route(e, Some(at)) {
                    return Ok(done);
                }
            }
            let expired = dl.expired();
            if let Err(e) = self.check_running() {
                self.stale = Some(at);
                return Err(e);
            }
            if expired {
                self.stale = Some(at);
                return Err(XhciError::Timeout);
            }
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn event_queue_keeps_order_and_counts_overflow() {
        let mut q = EventQueue::new();
        assert_eq!(q.pop(), None);
        for p in 0..EVENT_QUEUE_LEN as u8 + 3 {
            q.push(Event::PortStatusChange { port: p });
        }
        assert_eq!(q.dropped, 3);
        for p in 0..EVENT_QUEUE_LEN as u8 {
            assert_eq!(q.pop(), Some(Event::PortStatusChange { port: p }), "oldest first");
        }
        assert_eq!(q.pop(), None);
        // Wraps around the buffer.
        for p in 0..5 {
            q.push(Event::HostController { code: p });
            assert_eq!(q.pop(), Some(Event::HostController { code: p }));
        }
    }

    #[test_case]
    fn deadline_is_bounded_with_interrupts_off() {
        assert_eq!(cycles_for_ms(u64::MAX), u64::MAX, "saturates");
        x86_64::instructions::interrupts::without_interrupts(|| {
            let t0 = time::rdtsc();
            let d = Deadline::new(1);
            while !d.expired() {
                core::hint::spin_loop();
            }
            assert!(time::rdtsc().wrapping_sub(t0) >= cycles_for_ms(1));
        });
    }

    /// Without a controller (no xHCI in the default test machine) the wipe
    /// hook returns at once and commands fail cleanly.
    #[test_case]
    fn no_controller_is_harmless() {
        assert_eq!(usbsts(), None);
        halt_for_wipe();
        assert_eq!(command(Trb::noop_cmd(), 1), Err(XhciError::NotInitialized));
        assert_eq!(init(), Err(XhciError::NotFound));
        assert_eq!(caps(), None);
    }
}
