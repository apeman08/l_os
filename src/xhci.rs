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
//!
//! **Ports and devices** (spec §7.1 steps 5–6, Task 20): [`prepare_port`]
//! resets a connected root port if it is not enabled and acknowledges its
//! change bits (never writing PED), [`address_device`] enables a slot and
//! addresses the device, [`control_in`]/[`control_out_nodata`] run control
//! transfers on EP0, [`configure_endpoints`] adds interrupt/bulk
//! endpoints and [`bulk`] runs one bulk TD (Task 21, counted by
//! [`bulk_transfers`]). Every device has a fixed table entry ([`MAX_DEVICES`]) with
//! its own zeroed DMA frames (input and output context, EP0 ring, two
//! endpoint rings, one data page), kept for the kernel's lifetime and reused
//! after a controller reset. A transfer ring never has more than one control
//! TD in flight (the ring has no full check, ledger Task 12): every transfer
//! is waited for, and a ring whose transfer timed out or failed is not used
//! again. The enumeration itself (descriptors, classification) is
//! [`crate::usb`].

use crate::{memory, mmio, pci, time};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering, fence};
use l_core::usb_desc::{Endpoint, SetupPacket};
use l_core::xhci_ctx::{self as ctx, Caps, EndpointCtx, PortChange, PortSc, SlotCtx, Speed};
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
/// Devices the driver keeps state for (spec §7.1 step 6: fixed table).
pub const MAX_DEVICES: usize = 8;
/// TRBs per transfer ring (one page holds 256; 64 incl. the link TRB).
pub const TRANSFER_TRBS: usize = 64;
/// Bound of a control transfer (USB 2.0 §9.2.6.4 allows 5 s for a request
/// with data; QEMU and real keyboards/sticks answer in milliseconds).
pub const CONTROL_TIMEOUT_MS: u64 = 1000;
/// Bound of the wait for PRC after setting PR (USB 2.0 reset is 10–20 ms).
const PORT_RESET_MS: u64 = 500;
/// Reset recovery before the first request (USB 2.0 §7.1.7.5, TRSTRCY).
const RESET_RECOVERY_MS: u64 = 10;
/// Frames of one device's DMA area, in this order.
const DEV_FRAMES: usize = 6;
const PG_INPUT: u64 = 0;
const PG_OUTPUT: u64 = 1;
const PG_EP0: u64 = 2;
/// First of the two endpoint rings (`PG_EPS`, `PG_EPS + 1`).
const PG_EPS: u64 = 3;
const PG_BUF: u64 = 5;
/// Endpoint rings per device besides EP0 (bulk IN + OUT, or interrupt IN).
pub const MAX_ENDPOINTS: usize = 2;
/// Endpoint Context EP Type (xHCI 1.2 table 6-9).
const EP_TYPE_CONTROL: u8 = 4;
/// Bound of one bulk transfer (a stick may need a moment for its first
/// command after power-on; QEMU answers at once).
pub const BULK_TIMEOUT_MS: u64 = 3000;
/// CErr: three retries on transaction errors (section 4.10.2.7).
const CERR: u8 = 3;
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
    /// Port number outside `1..=MaxPorts`.
    BadPort,
    /// Slot not in the device table (or a slot ID the controller should not
    /// have handed out).
    BadSlot,
    /// Buffer outside the slot's data page, wrong length or direction.
    BadBuffer,
    /// Endpoint not usable (type, address, max packet, count, duplicate or
    /// already configured).
    BadEndpoint,
    /// The device table ([`MAX_DEVICES`]) is full.
    TableFull,
    /// A device command completed with this (non-success) code.
    Command(u8),
    /// A transfer completed with this (error) code; the endpoint is not used
    /// again.
    Transfer(u8),
    /// An earlier transfer on this endpoint timed out or failed; its ring is
    /// in an unknown state and is not used again.
    EndpointBroken,
    /// Bulk transfers are sealed ([`seal_bulk`], spec §7.1 step 8): nothing
    /// was enqueued.
    Sealed,
}

/// Result of [`prepare_port`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortReady {
    /// Nothing connected (change bits acknowledged).
    Empty,
    /// Connected and enabled at this speed, change bits acknowledged.
    Ready(Speed),
    /// Connected and enabled, but the speed ID (raw PSIV) is not in the
    /// default table (ledger Task 13: reported, not guessed).
    UnknownSpeed(u8),
    /// The device went away or reconnected during the reset, or the port
    /// did not enable; nothing is addressed this round.
    Lost,
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

/// The DMA structures (physical addresses; each one contiguous run). The
/// scratchpad buffers (one page each, not contiguous) are listed in
/// [`Driver::scratch`], a software copy: the array in DMA memory is never
/// read back (a hostile controller could have overwritten it).
#[derive(Debug, Clone, Copy)]
struct Dma {
    dcbaa: u64,
    /// Scratchpad array and its frame count (0 if no scratchpad).
    scratch_array: u64,
    scratch_array_frames: usize,
    cmd: u64,
    event: u64,
    erst: u64,
}

impl Dma {
    fn runs(&self) -> [(u64, usize); 5] {
        [
            (self.dcbaa, 1),
            (self.scratch_array, self.scratch_array_frames),
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

/// Outcome of [`drain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drained {
    /// `route` accepted this event (the awaited completion).
    Done(Event),
    /// The source had no further event.
    Empty,
    /// `max` events were consumed and more may follow.
    Limit,
}

/// Hand at most `max` events from `next` to `route`; stops early when the
/// source is empty or `route` returns the awaited event. Bounded whatever
/// the (hardware) source does.
fn drain<E>(
    max: usize,
    mut next: impl FnMut() -> Result<Option<Event>, E>,
    mut route: impl FnMut(Event) -> Option<Event>,
) -> Result<Drained, E> {
    for _ in 0..max {
        match next()? {
            None => return Ok(Drained::Empty),
            Some(e) => {
                if let Some(done) = route(e) {
                    return Ok(Drained::Done(done));
                }
            }
        }
    }
    Ok(Drained::Limit)
}

/// What a waiter is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    /// Nothing: every event is routed elsewhere.
    Nothing,
    /// The completion of the command TRB at this physical address.
    Command(u64),
    /// Any transfer event of this slot and endpoint (DCI); the waiter checks
    /// which TRB it names.
    Transfer { slot: u8, dci: u8 },
}

/// Route one event: `Some(event)` if it is what `want` waits for; the
/// completion of the `stale` (timed-out) command clears it; any other
/// completion (of nothing we know) is counted as dropped; every other
/// event is queued for [`pop_event`].
fn route(queue: &mut EventQueue, stale: &mut Option<u64>, e: Event, want: Want) -> Option<Event> {
    match e {
        Event::CommandCompletion { trb, .. } if want == Want::Command(trb) => return Some(e),
        Event::CommandCompletion { trb, .. } if Some(trb) == *stale => *stale = None,
        Event::CommandCompletion { .. } => queue.dropped = queue.dropped.saturating_add(1),
        Event::Transfer { slot, endpoint, .. } if want == (Want::Transfer { slot, dci: endpoint }) => return Some(e),
        other => queue.push(other),
    }
    None
}

/// One addressed device (spec §7.1 step 6).
#[derive(Debug, Clone, Copy)]
struct Device {
    slot: u8,
    port: u8,
    speed: Speed,
    /// Physical address of its [`DEV_FRAMES`] contiguous frames.
    frames: u64,
    ep0: ProducerRing,
    /// EP0 is not used again (a transfer timed out or failed).
    ep0_broken: bool,
    /// Configured endpoints: DCI and ring ([`configure_endpoints`]).
    eps: [Option<(u8, ProducerRing)>; MAX_ENDPOINTS],
    /// Endpoint `eps[k]` is not used again (a bulk transfer timed out,
    /// stalled or failed; no Reset Endpoint recovery exists).
    eps_broken: [bool; MAX_ENDPOINTS],
    /// Endpoint `eps[k]` is a bulk endpoint (else interrupt).
    bulk: [bool; MAX_ENDPOINTS],
}

impl Device {
    fn page(&self, n: u64) -> u64 {
        self.frames + n * PAGE
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
    /// Physical addresses of the scratchpad buffer pages.
    scratch: Vec<u64>,
    /// Device table; reset by every `start` (the controller reset disabled
    /// every slot).
    devices: [Option<Device>; MAX_DEVICES],
    /// DMA area of table entry `i` (0 = not allocated yet). Allocated on
    /// first use, kept for the kernel's lifetime, re-zeroed on reuse.
    dev_frames: [u64; MAX_DEVICES],
    /// Number of successful `start`s; device state belongs to one.
    generation: u64,
}

static DRIVER: Mutex<Option<Driver>> = Mutex::new(None);

/// Lock-free copy of the operational registers' virtual address (0 = no
/// controller mapped), for [`halt_for_wipe`], which must not take
/// [`DRIVER`] (spec E14, like the PCI/ACPI mirrors of Task 10). Published
/// once in `probe`, after the mapping exists and the offsets were checked;
/// the window's mappings are never removed.
static OP_VIRT: AtomicU64 = AtomicU64::new(0);
/// Lock-free copy of `MaxPorts` (published with [`OP_VIRT`], checked
/// against the mapping in `probe`), for [`portsc_unlocked`].
static MAX_PORTS: AtomicU8 = AtomicU8::new(0);
/// Bulk TDs ever enqueued (rung doorbells on bulk endpoints), for the
/// "no transfer after the seal" checks of `usb::storage_seal`.
static BULK_TDS: AtomicU64 = AtomicU64::new(0);
/// Set once by [`seal_bulk`], never cleared (also not by a re-[`init`]).
static BULK_SEALED: AtomicBool = AtomicBool::new(false);

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
///
/// [`caps`], [`command_ring`], [`pop_event`], [`dropped_events`], [`init`]
/// and [`command`] take the driver lock (spinning): never call them from an
/// ISR. Task 20's timer-ISR polling must use `try_lock` (or own the lock
/// with IF=0); only [`usbsts`], [`usbcmd`] and [`halt_for_wipe`] are
/// lock-free.
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
/// while the controller runs normally; after a failure, a halt or a command
/// that timed out without completing ([`XhciError::Busy`]) it resets the
/// controller again, reusing the mapping and the DMA frames. Takes the
/// driver lock: not for ISR context.
pub fn init() -> Result<(), XhciError> {
    let mut g = DRIVER.lock();
    if let Some(d) = g.as_ref() {
        // A command that timed out and never completed (`Busy`) is not
        // "running": init resets the controller and the rings again.
        if d.started && d.stale.is_none() && d.sts().is_ok_and(|s| s & STS_HCH == 0) {
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

/// Frames taken for one first-time DMA setup; [`Frames::free`] gives them
/// all back if a later step fails (nothing is leaked).
struct Frames(Vec<(u64, usize)>);

impl Frames {
    /// `n` contiguous zeroed frames, below 4 GiB unless `ac64`.
    fn take(&mut self, n: usize, ac64: bool) -> Result<u64, XhciError> {
        self.0.try_reserve(1).map_err(|_| XhciError::NoMemory)?;
        let phys = {
            let mut g = memory::MEMORY.lock();
            let mem = g.as_mut().ok_or(XhciError::NoMemory)?;
            mem.frames.allocate_contiguous(n).map(|f| f.start_address().as_u64()).ok_or(XhciError::NoMemory)?
        };
        self.0.push((phys, n));
        if !ac64 && phys + n as u64 * PAGE > FOUR_GIB {
            return Err(XhciError::NoMemory);
        }
        Ok(phys)
    }

    fn free(self) {
        use x86_64::structures::paging::{FrameDeallocator, PhysFrame};
        let mut g = memory::MEMORY.lock();
        let Some(mem) = g.as_mut() else { return };
        for (phys, n) in self.0 {
            for i in 0..n as u64 {
                let f = PhysFrame::containing_address(x86_64::PhysAddr::new(phys + i * PAGE));
                // SAFETY: the frame was allocated by `take` during this
                // setup attempt and its address was never written to a
                // controller register, so neither the controller nor any
                // Rust reference uses it.
                unsafe { mem.frames.deallocate_frame(f) };
            }
        }
    }
}

/// Virtual address of DMA memory at physical `phys` (offset mapping).
fn dma_virt(phys: u64) -> u64 {
    (memory::phys_offset() + phys).as_u64()
}

/// Write one 32-bit word of DMA memory.
///
/// # Safety
/// `phys..phys+4` must lie in a frame this driver allocated and owns for
/// the kernel's lifetime (its [`Dma`] structures or a device area in
/// `Driver::dev_frames`), 4-byte aligned.
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
/// `phys` must be a TRB slot (16 bytes) of one of the driver's rings
/// (command ring or a transfer ring in a device area).
unsafe fn write_trb(phys: u64, t: Trb) {
    let w = t.to_words();
    // SAFETY: caller contract: the 16 bytes at `phys` are a slot of one of
    // our rings, inside a driver-owned frame.
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
        MAX_PORTS.store(caps.max_ports, Ordering::Release);
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
            scratch: Vec::new(),
            devices: [None; MAX_DEVICES],
            dev_frames: [0; MAX_DEVICES],
            generation: 0,
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
        // HCRST disabled every slot and the DCBAA was re-zeroed: no device
        // state survives (the frames stay in `dev_frames` for reuse).
        self.devices = [None; MAX_DEVICES];

        // All DMA structures are written before the controller may run.
        fence(Ordering::SeqCst);
        let c = self.read_op(USBCMD)?;
        self.write_op(USBCMD, c & !(CMD_INTE | CMD_HCRST) | CMD_RS)?;
        self.wait(t, |d| Ok(d.sts()? & STS_HCH == 0))?;
        self.started = true;
        self.generation = self.generation.wrapping_add(1);

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
                let pages = self.scratch.iter().map(|&p| (p, 1));
                for (phys, n) in dma.runs().into_iter().chain(pages) {
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
                let mut frames = Frames(Vec::new());
                match Driver::allocate(&mut frames, caps) {
                    Ok((dma, scratch)) => {
                        // Kept for the kernel's lifetime; a restart reuses them.
                        self.dma = Some(dma);
                        self.scratch = scratch;
                        dma
                    }
                    Err(e) => {
                        frames.free();
                        return Err(e);
                    }
                }
            }
        };
        // SAFETY: every address below lies in the frames of `dma` (sizes
        // computed above: DCBAA entry 0 in its frame, `n` 8-byte entries in
        // the scratchpad array, the ERST entry in its frame), 4-byte aligned.
        unsafe {
            if !self.scratch.is_empty() {
                for (i, &buf) in self.scratch.iter().enumerate() {
                    let i = i as u64;
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

    /// First-time allocation of every DMA structure (each zeroed); on error
    /// the caller frees what `frames` holds.
    fn allocate(frames: &mut Frames, caps: Caps) -> Result<(Dma, Vec<u64>), XhciError> {
        let ac64 = caps.ac64;
        let n = usize::from(caps.max_scratchpad);
        let array_frames = (n * 8).div_ceil(PAGE as usize);
        let dma = Dma {
            dcbaa: frames.take(1, ac64)?,
            scratch_array: if n > 0 { frames.take(array_frames, ac64)? } else { 0 },
            scratch_array_frames: array_frames,
            cmd: frames.take(1, ac64)?,
            event: frames.take(1, ac64)?,
            erst: frames.take(1, ac64)?,
        };
        let mut scratch = Vec::new();
        scratch.try_reserve_exact(n).map_err(|_| XhciError::NoMemory)?;
        for _ in 0..n {
            // One page each: scratchpad buffers need not be contiguous.
            scratch.push(frames.take(1, ac64)?);
        }
        Ok((dma, scratch))
    }

    /// Consume one event TRB, if the controller produced one, and move
    /// ERDP past it (EHB written as 1 to clear it).
    fn next_event(ev: &mut EventRing, regs: &mmio::Mmio, ir0: u64, dma: &Dma) -> Result<Option<Event>, XhciError> {
        let base = dma.event;
        let t = ev.poll(|i| {
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
        regs.write64(ir0 + ERDP, ev.erdp(base))?;
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

    /// One bounded pass over the event ring (at most [`EVENT_TRBS`] events,
    /// so a controller that keeps producing events cannot keep the caller
    /// from checking its deadline).
    fn drain_once(&mut self, dma: &Dma, want: Want) -> Result<Drained, XhciError> {
        let Driver { ev, regs, ir0, queue, stale, .. } = self;
        let ir0 = *ir0;
        drain(EVENT_TRBS, || Driver::next_event(ev, regs, ir0, dma), |e| route(queue, stale, e, want))
    }

    fn submit(&mut self, trb: Trb, timeout_ms: u64) -> Result<Event, XhciError> {
        if !COMMAND_TYPES.contains(&trb.trb_type()) {
            return Err(XhciError::InvalidTrb);
        }
        let dma = self.dma.ok_or(XhciError::NotInitialized)?;
        self.check_running()?;
        // At most one command in flight (the ring has no full check).
        if self.stale.is_some() {
            self.drain_once(&dma, Want::Nothing)?;
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
            if let Drained::Done(e) = self.drain_once(&dma, Want::Command(at))? {
                return Ok(e);
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

/// Run `f` on the started driver (lock held: boot phase only, not for an
/// ISR).
fn with_driver<T>(f: impl FnOnce(&mut Driver) -> Result<T, XhciError>) -> Result<T, XhciError> {
    let mut g = DRIVER.lock();
    match g.as_mut() {
        Some(d) if d.started => f(d),
        _ => Err(XhciError::NotInitialized),
    }
}

/// Number of successful controller starts, `None` before the first one.
/// Device state ([`address_device`] and later) belongs to one generation:
/// a re-[`init`] resets the controller and forgets every slot.
pub fn generation() -> Option<u64> {
    DRIVER.lock().as_ref().filter(|d| d.started).map(|d| d.generation)
}

/// `MaxPorts`, 0 before a controller was mapped (lock-free).
pub fn port_count() -> u8 {
    MAX_PORTS.load(Ordering::Acquire)
}

/// Raw PORTSC of root port `port` (1-based).
pub fn portsc(port: u8) -> Result<PortSc, XhciError> {
    with_driver(|d| d.read_port(d.port_off(port)?))
}

/// PORTSC of root port `port` without the driver lock (for the timer-ISR
/// polling of Tasks 21–23, which must not spin on [`DRIVER`]); `None`
/// before a controller was mapped or for a port outside `1..=MaxPorts`.
/// An all-ones value means the controller is gone; the caller checks
/// [`PortSc::is_all_ones`] before acting on it.
pub fn portsc_unlocked(port: u8) -> Option<PortSc> {
    let op = OP_VIRT.load(Ordering::Acquire);
    if op == 0 || port == 0 || port > MAX_PORTS.load(Ordering::Acquire) {
        return None;
    }
    let off = PORT_REGS + 0x10 * u64::from(port - 1);
    // SAFETY: `op` was published by `probe` after it checked that the port
    // registers of all `MaxPorts` ports lie inside the BAR mapping (which is
    // never unmapped); `port` is in `1..=MaxPorts`, so `op + off` is the
    // 4-byte aligned PORTSC of that port.
    Some(PortSc(unsafe { op_read(op, off) }))
}

/// Spec §7.1 step 5 for one root port: a connected port that is not enabled
/// (USB2 after HCRST) or whose speed is not known is reset (PR) and PRC
/// awaited; then the change bits are acknowledged (never PED) and the port
/// re-read, so a disconnect or reconnect during the reset is seen
/// ([`PortReady::Lost`]). An empty port only gets its change bits
/// acknowledged. Errors are controller-level (gone, bad port number).
pub fn prepare_port(port: u8) -> Result<PortReady, XhciError> {
    with_driver(|d| d.prepare_port(port))
}

/// Acknowledge whatever change bits root port `port` has set (writing only
/// those bits, PED as 0) and return PORTSC as read afterwards.
pub fn ack_port(port: u8) -> Result<PortSc, XhciError> {
    with_driver(|d| {
        let off = d.port_off(port)?;
        let v = d.read_port(off)?;
        d.ack_port(off, v)?;
        d.read_port(off)
    })
}

/// Enable Slot, set up the slot's input/output contexts and EP0 ring
/// (EP0 max packet from `speed`), point DCBAA at the output context and
/// Address Device (BSR = 0). Returns the slot ID.
pub fn address_device(port: u8, speed: Speed) -> Result<u8, XhciError> {
    with_driver(|d| d.address_device(port, speed))
}

/// Evaluate Context: EP0 max packet size `max_packet` (after the 8-byte
/// device-descriptor prefix showed a different `bMaxPacketSize0`).
pub fn set_ep0_max_packet(slot: u8, max_packet: u16) -> Result<(), XhciError> {
    with_driver(|d| d.set_ep0_max_packet(slot, max_packet))
}

/// Physical address of the slot's data page (one page, for the data stage
/// of [`control_in`]); `None` for a slot not in the table.
pub fn slot_buffer(slot: u8) -> Option<u64> {
    let g = DRIVER.lock();
    let d = g.as_ref().filter(|d| d.started)?;
    d.device(slot).ok().map(|(_, dev)| dev.page(PG_BUF))
}

/// Copy `out.len()` bytes from offset `off` of the slot's data page.
pub fn read_slot_buffer(slot: u8, off: usize, out: &mut [u8]) -> Result<(), XhciError> {
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        let end = off.checked_add(out.len()).ok_or(XhciError::BadBuffer)?;
        if end > PAGE as usize {
            return Err(XhciError::BadBuffer);
        }
        let base = dma_virt(dev.page(PG_BUF) + off as u64) as *const u8;
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: `off + i < PAGE`: a byte of the slot's data page, a
            // frame this driver owns. No transfer is in flight (they are
            // waited for under the lock we hold), and a device write would
            // only change the value, so a volatile read of a `u8` is sound.
            *b = unsafe { core::ptr::read_volatile(base.add(i)) };
        }
        Ok(())
    })
}

/// Copy `data` to offset `off` of the slot's data page (e.g. a CBW before
/// [`bulk`] sends it).
pub fn write_slot_buffer(slot: u8, off: usize, data: &[u8]) -> Result<(), XhciError> {
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        let end = off.checked_add(data.len()).ok_or(XhciError::BadBuffer)?;
        if end > PAGE as usize {
            return Err(XhciError::BadBuffer);
        }
        let base = dma_virt(dev.page(PG_BUF) + off as u64) as *mut u8;
        for (i, &b) in data.iter().enumerate() {
            // SAFETY: `off + i < PAGE`: a byte of the slot's data page, a
            // frame this driver owns and no Rust reference points into. No
            // transfer is in flight (every transfer is waited for under the
            // lock we hold; a broken endpoint's ring is never rung again).
            unsafe { core::ptr::write_volatile(base.add(i), b) };
        }
        Ok(())
    })
}

/// One bulk TD (a single Normal TRB with ISP and IOC) of `len` bytes on the
/// configured bulk endpoint `ep_address` (bit 7 = IN) of `slot`, using
/// `buf_phys`, which must lie in the slot's data page ([`slot_buffer`]).
/// Returns the bytes transferred (fewer on a short packet, code 13, which
/// is not an error). Exactly one TD is in flight on the ring (ledger
/// Task 12). Waits at most [`BULK_TIMEOUT_MS`]; a timeout or an error
/// completion (e.g. a stall, code 6) leaves the endpoint unused from then
/// on (there is no Reset Endpoint recovery).
///
/// After [`seal_bulk`] every call returns [`XhciError::Sealed`] before
/// anything else is looked at. Bulk endpoints exist only on mass storage
/// (`usb_desc::classify`; the keyboard has an interrupt endpoint) and this
/// is the only place that enqueues a bulk TD, so the seal here is the
/// lowest-level guarantee that no storage device is read again, whoever
/// the caller is.
pub fn bulk(slot: u8, ep_address: u8, buf_phys: u64, len: usize) -> Result<usize, XhciError> {
    if bulk_sealed() {
        return Err(XhciError::Sealed);
    }
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        let page = dev.page(PG_BUF);
        let fits = buf_phys >= page && buf_phys.checked_add(len as u64).is_some_and(|end| end <= page + PAGE);
        // Endpoint first: an unknown endpoint is BadEndpoint whatever the buffer.
        d.bulk_ring(&dev, ep_address)?;
        if len == 0 || !fits {
            return Err(XhciError::BadBuffer);
        }
        d.bulk(slot, ep_address, buf_phys, len as u32)
    })
}

/// Refuse every later bulk transfer, for good ([`bulk`] returns
/// [`XhciError::Sealed`]; spec §7.1 step 8). Called by `usb::storage_seal`.
/// Takes the driver lock, so a transfer in progress (which holds it while
/// it waits) has completed when this returns, and every later one sees the
/// flag under the lock ([`Driver::bulk`] checks it again). Thread context
/// only, like every locked driver call.
pub fn seal_bulk() {
    let _g = DRIVER.lock();
    BULK_SEALED.store(true, Ordering::Release);
}

/// [`seal_bulk`] was called (lock-free).
pub fn bulk_sealed() -> bool {
    BULK_SEALED.load(Ordering::Acquire)
}

/// Bulk TDs enqueued since boot (every one rang a doorbell).
pub fn bulk_transfers() -> u64 {
    BULK_TDS.load(Ordering::Acquire)
}

/// Control transfer with an IN data stage of `len` bytes into `buf_phys`,
/// which must lie in the slot's data page ([`slot_buffer`]); `len` must
/// equal `packet.length`. Returns the bytes received (fewer on a short
/// packet). Waits at most [`CONTROL_TIMEOUT_MS`]; a timeout or an error
/// completion leaves EP0 unused from then on.
pub fn control_in(slot: u8, packet: SetupPacket, buf_phys: u64, len: usize) -> Result<usize, XhciError> {
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        let page = dev.page(PG_BUF);
        let fits = buf_phys >= page && buf_phys.checked_add(len as u64).is_some_and(|end| end <= page + PAGE);
        if !packet.dir_in() || len == 0 || len != usize::from(packet.length) || !fits {
            return Err(XhciError::BadBuffer);
        }
        d.control(slot, packet, Some((buf_phys, len as u32)))
    })
}

/// Control transfer without a data stage (`packet` must be host-to-device
/// with `wLength == 0`), e.g. SET_CONFIGURATION, SET_PROTOCOL, SET_IDLE.
pub fn control_out_nodata(slot: u8, packet: SetupPacket) -> Result<(), XhciError> {
    with_driver(|d| {
        d.device(slot)?;
        if packet.dir_in() || packet.length != 0 {
            return Err(XhciError::BadBuffer);
        }
        d.control(slot, packet, None).map(|_| ())
    })
}

/// Configure Endpoint: add up to [`MAX_ENDPOINTS`] interrupt or bulk
/// endpoints, each with a fresh transfer ring (link TRB back to its start,
/// dequeue pointer = ring start, DCS = 1) and, for interrupt endpoints, the
/// interval from `ep_interval(speed, bInterval)`. Once per device.
pub fn configure_endpoints(slot: u8, eps: &[(Endpoint, Speed)]) -> Result<(), XhciError> {
    with_driver(|d| d.configure_endpoints(slot, eps))
}

/// Slot State from the output Slot Context (0 disabled/enabled,
/// 1 default, 2 addressed, 3 configured); `None` for an unknown slot.
pub fn slot_state(slot: u8) -> Option<u8> {
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        // SAFETY: DW3 of the slot context, inside the slot's output-context
        // frame (owned by the driver), 4-byte aligned.
        Ok((unsafe { dma_read32(dev.page(PG_OUTPUT) + 12) } >> 27) as u8)
    })
    .ok()
}

/// EP State of endpoint `dci` (1..=31) from the output context
/// (0 disabled, 1 running, 2 halted, 3 stopped, 4 error).
pub fn endpoint_state(slot: u8, dci: u8) -> Option<u8> {
    if !(1..=31).contains(&dci) {
        return None;
    }
    with_driver(|d| {
        let (_, dev) = d.device(slot)?;
        let at = dev.page(PG_OUTPUT) + ctx::ctx_offset(usize::from(dci), d.caps.csz64) as u64;
        // SAFETY: `dci <= 31`, so the context lies inside the 2 KiB (64-byte
        // contexts) output context in the slot's own frame; 4-byte aligned.
        Ok((unsafe { dma_read32(at) } & 7) as u8)
    })
    .ok()
}

/// Write one context (8 dwords) of a device area.
///
/// # Safety
/// `phys..phys+32` must lie in a frame of a device area of this driver.
unsafe fn write_ctx(phys: u64, words: [u32; 8]) {
    for (i, w) in words.into_iter().enumerate() {
        // SAFETY: caller contract; `i < 8`.
        unsafe { dma_write32(phys + 4 * i as u64, w) };
    }
}

/// Zero one frame of a device area.
///
/// # Safety
/// `phys` must be a frame of a device area of this driver that the
/// controller does not use right now (no command or transfer naming it is
/// in flight).
unsafe fn zero_frame(phys: u64) {
    // SAFETY: caller contract; the frame is RAM mapped at the offset and no
    // Rust reference to it exists.
    unsafe { core::ptr::write_bytes(dma_virt(phys) as *mut u8, 0, PAGE as usize) };
}

/// Spin at most `ms` milliseconds (tick and TSC bounded).
pub(crate) fn pause_ms(ms: u64) {
    let dl = Deadline::new(ms);
    while !dl.expired() {
        core::hint::spin_loop();
    }
}

impl Driver {
    fn port_off(&self, port: u8) -> Result<u64, XhciError> {
        if port == 0 || port > self.caps.max_ports {
            return Err(XhciError::BadPort);
        }
        Ok(self.op + PORT_REGS + 0x10 * u64::from(port - 1))
    }

    /// PORTSC at `off`; all-ones (controller gone) is an error, checked
    /// before anything acts on the value (ledger Task 13).
    fn read_port(&self, off: u64) -> Result<PortSc, XhciError> {
        let v = PortSc(self.regs.read32(off)?);
        if v.is_all_ones() {
            return Err(XhciError::ControllerGone);
        }
        Ok(v)
    }

    /// Write the acknowledgement for the change bits set in `v` (only those;
    /// PED as 0). No write if none is set.
    fn ack_port(&self, off: u64, v: PortSc) -> Result<(), XhciError> {
        let w = v.ack_changes();
        if w & 0x00FE_0000 != 0 {
            self.regs.write32(off, w)?;
        }
        Ok(())
    }

    fn prepare_port(&mut self, port: u8) -> Result<PortReady, XhciError> {
        let off = self.port_off(port)?;
        let v = self.read_port(off)?;
        if !v.ccs() {
            self.ack_port(off, v)?;
            return Ok(PortReady::Empty);
        }
        // USB3 ports are enabled by link training (spec §7.1 step 5); a
        // USB2 port after HCRST is not. A connected port with an unknown
        // speed ID is reset too instead of acking its CSC blindly (ledger
        // Task 13): the reset is how the speed is learned.
        if !v.ped() || Speed::from_psi(v.speed()).is_none() {
            self.regs.write32(off, v.reset_request())?;
            let mut last = v;
            let r = self.wait(PORT_RESET_MS, |d| {
                last = d.read_port(off)?;
                Ok(last.prc() || !last.ccs())
            });
            match r {
                Ok(()) => {}
                Err(XhciError::Timeout) => {
                    self.ack_port(off, last)?;
                    return Ok(PortReady::Lost);
                }
                Err(e) => return Err(e),
            }
            match ctx::classify(true, last) {
                PortChange::ResetDone => {}
                PortChange::ControllerGone => return Err(XhciError::ControllerGone),
                _ => {
                    self.ack_port(off, last)?;
                    return Ok(PortReady::Lost);
                }
            }
            pause_ms(RESET_RECOVERY_MS);
        }
        let v = self.read_port(off)?;
        self.ack_port(off, v)?;
        // Re-read after the acknowledgement: a disconnect or a reconnect
        // (new CSC) during the reset means the device we reset is not the
        // one there now; leave it for the port-change handling.
        let w = self.read_port(off)?;
        if !w.ccs() || !w.ped() || w.csc() {
            return Ok(PortReady::Lost);
        }
        Ok(match Speed::from_psi(w.speed()) {
            Some(s) => PortReady::Ready(s),
            None => PortReady::UnknownSpeed(w.speed()),
        })
    }

    /// Table entry and copy of the device in `slot`.
    fn device(&self, slot: u8) -> Result<(usize, Device), XhciError> {
        self.devices
            .iter()
            .enumerate()
            .find_map(|(i, d)| d.filter(|d| d.slot == slot && slot != 0).map(|d| (i, d)))
            .ok_or(XhciError::BadSlot)
    }

    /// No timed-out command may still be pending before a device structure
    /// is rewritten (the controller could still be reading it).
    fn idle(&mut self) -> Result<Dma, XhciError> {
        let dma = self.dma.ok_or(XhciError::NotInitialized)?;
        self.check_running()?;
        if self.stale.is_some() {
            self.drain_once(&dma, Want::Nothing)?;
            if self.stale.is_some() {
                return Err(XhciError::Busy);
            }
        }
        Ok(dma)
    }

    /// The DMA area of table entry `i`: allocated on first use, re-zeroed
    /// when reused (the entry is free, so no slot of this generation and,
    /// after HCRST, no earlier one points at it).
    fn device_frames(&mut self, i: usize) -> Result<u64, XhciError> {
        let phys = self.dev_frames[i];
        if phys != 0 {
            for n in 0..DEV_FRAMES as u64 {
                // SAFETY: frame `n` of entry `i`'s area, owned by the
                // driver; the entry is free (see above), so the controller
                // does not use it.
                unsafe { zero_frame(phys + n * PAGE) };
            }
            return Ok(phys);
        }
        let mut frames = Frames(Vec::new());
        match frames.take(DEV_FRAMES, self.caps.ac64) {
            Ok(phys) => {
                self.dev_frames[i] = phys;
                Ok(phys)
            }
            Err(e) => {
                frames.free();
                Err(e)
            }
        }
    }

    fn command_ok(&mut self, trb: Trb) -> Result<u8, XhciError> {
        match self.submit(trb, DEFAULT_TIMEOUT_MS)? {
            Event::CommandCompletion { code: ring::SUCCESS, slot, .. } => Ok(slot),
            Event::CommandCompletion { code, .. } => Err(XhciError::Command(code)),
            _ => Err(XhciError::Command(0)),
        }
    }

    fn ep0_ctx(ring: u64, max_packet: u16) -> [u32; 8] {
        EndpointCtx {
            ep_type: EP_TYPE_CONTROL,
            max_packet,
            interval: 0,
            cerr: CERR,
            dequeue: ring,
            dcs: true,
            avg_trb_len: 8,
        }
        .encode()
    }

    fn address_device(&mut self, port: u8, speed: Speed) -> Result<u8, XhciError> {
        self.port_off(port)?;
        let dma = self.idle()?;
        let i = self.devices.iter().position(Option::is_none).ok_or(XhciError::TableFull)?;
        let frames = self.device_frames(i)?;
        let slot = self.command_ok(Trb::enable_slot())?;
        if slot == 0 || slot > self.caps.max_slots || self.devices.iter().flatten().any(|d| d.slot == slot) {
            return Err(XhciError::BadSlot);
        }
        let dev = Device {
            slot,
            port,
            speed,
            frames,
            ep0: ProducerRing::new(frames + PG_EP0 * PAGE, TRANSFER_TRBS),
            ep0_broken: false,
            eps: [None; MAX_ENDPOINTS],
            eps_broken: [false; MAX_ENDPOINTS],
            bulk: [false; MAX_ENDPOINTS],
        };
        let csz = self.caps.csz64;
        let input = dev.page(PG_INPUT);
        let off = |n: usize| ctx::ctx_offset(n, csz) as u64;
        // SAFETY: input control, slot and EP0 context (indices 0..=2, at
        // most 3 * 64 bytes) lie in the entry's input-context frame; the
        // DCBAA entry `slot <= MaxSlots <= 255` (8 bytes each) lies in the
        // DCBAA frame. The area was zeroed by `device_frames` and no command
        // naming it is in flight (`idle`).
        unsafe {
            write_ctx(input + off(0), ctx::input_control(0b11, 0));
            write_ctx(input + off(1), SlotCtx { route: 0, speed, context_entries: 1, root_port: port }.encode());
            write_ctx(input + off(2), Driver::ep0_ctx(dev.ep0.enqueue_ptr(), speed.ep0_max_packet()));
            let out = dev.page(PG_OUTPUT);
            let e = dma.dcbaa + 8 * u64::from(slot);
            dma_write32(e, out as u32);
            dma_write32(e + 4, (out >> 32) as u32);
        }
        // From here the controller may write the output context: the entry
        // is taken (and its frames never reused in this generation) even if
        // Address Device fails.
        self.devices[i] = Some(dev);
        fence(Ordering::SeqCst);
        self.command_ok(Trb::address_device(input, slot, false))?;
        Ok(slot)
    }

    fn set_ep0_max_packet(&mut self, slot: u8, max_packet: u16) -> Result<(), XhciError> {
        let (_, dev) = self.device(slot)?;
        if max_packet == 0 {
            return Err(XhciError::BadEndpoint);
        }
        self.idle()?;
        let input = dev.page(PG_INPUT);
        let off = |n: usize| ctx::ctx_offset(n, self.caps.csz64) as u64;
        // SAFETY: the input-context frame of this device, not in use (no
        // command in flight: `idle`); contexts 0 and 2 lie inside it.
        unsafe {
            zero_frame(input);
            write_ctx(input + off(0), ctx::input_control(0b10, 0));
            write_ctx(input + off(2), Driver::ep0_ctx(dev.ep0.enqueue_ptr(), max_packet));
        }
        fence(Ordering::SeqCst);
        self.command_ok(Trb::evaluate_context(input, slot)).map(|_| ())
    }

    fn configure_endpoints(&mut self, slot: u8, eps: &[(Endpoint, Speed)]) -> Result<(), XhciError> {
        let (i, mut dev) = self.device(slot)?;
        if eps.is_empty() || eps.len() > MAX_ENDPOINTS || dev.eps.iter().any(Option::is_some) {
            return Err(XhciError::BadEndpoint);
        }
        // Validate everything before touching DMA memory.
        let mut plan = [(0u8, [0u32; 8]); MAX_ENDPOINTS];
        for (k, &(ep, speed)) in eps.iter().enumerate() {
            let dci = ctx::dci(ep.address);
            let dir_in = ep.address & 0x80 != 0;
            let ep_type = match (ep.attributes & 3, dir_in) {
                (2, false) => 2,
                (3, false) => 3,
                (2, true) => 6,
                (3, true) => 7,
                _ => return Err(XhciError::BadEndpoint),
            };
            let max_packet = ep.max_packet & 0x7FF;
            if dci < 2 || max_packet == 0 || plan[..k].iter().any(|p| p.0 == dci) {
                return Err(XhciError::BadEndpoint);
            }
            let interrupt = ep.attributes & 3 == 3;
            let ring = dev.page(PG_EPS + k as u64);
            let mut w = EndpointCtx {
                ep_type,
                max_packet,
                interval: if interrupt { ctx::ep_interval(speed, ep.interval) } else { 0 },
                cerr: CERR,
                dequeue: ring,
                dcs: true,
                avg_trb_len: max_packet,
            }
            .encode();
            if interrupt {
                // HS high-bandwidth: additional transactions per microframe
                // (wMaxPacketSize 12:11) as Max Burst; Max ESIT Payload Lo
                // (DW4 31:16) = bytes per service interval (section 6.2.3).
                let mult = if speed == Speed::High { u32::from(ep.max_packet >> 11 & 3) } else { 0 };
                w[1] |= mult << 8;
                w[4] |= (u32::from(max_packet) * (mult + 1)) << 16;
            }
            plan[k] = (dci, w);
        }
        self.idle()?;
        let csz = self.caps.csz64;
        let input = dev.page(PG_INPUT);
        let off = |n: usize| ctx::ctx_offset(n, csz) as u64;
        let max_dci = plan[..eps.len()].iter().map(|p| p.0).max().unwrap_or(1);
        let add = plan[..eps.len()].iter().fold(1u32, |a, p| a | 1 << p.0);
        // SAFETY: the input-context frame and the endpoint-ring frames of
        // this device; no command names them right now (`idle`) and the
        // endpoints do not exist yet, so the controller does not read the
        // rings. Context index `dci + 1 <= 32` lies inside the input
        // context (33 contexts of at most 64 bytes = 2112 bytes < 4 KiB).
        unsafe {
            zero_frame(input);
            write_ctx(input + off(0), ctx::input_control(add, 0));
            let sc = SlotCtx { route: 0, speed: dev.speed, context_entries: max_dci, root_port: dev.port };
            write_ctx(input + off(1), sc.encode());
            for (k, &(dci, w)) in plan[..eps.len()].iter().enumerate() {
                zero_frame(dev.page(PG_EPS + k as u64));
                write_ctx(input + off(usize::from(dci) + 1), w);
            }
        }
        fence(Ordering::SeqCst);
        self.command_ok(Trb::configure_endpoint(input, slot))?;
        for (k, &(dci, _)) in plan[..eps.len()].iter().enumerate() {
            dev.eps[k] = Some((dci, ProducerRing::new(dev.page(PG_EPS + k as u64), TRANSFER_TRBS)));
            dev.bulk[k] = eps[k].0.attributes & 3 == 2;
        }
        self.devices[i] = Some(dev);
        Ok(())
    }

    /// Index into `dev.eps` of the configured *bulk* endpoint `ep_address`
    /// (an interrupt endpoint, EP0 or an unconfigured address is
    /// `BadEndpoint`). Read from the driver's software copy of the input
    /// context it wrote, never from controller memory.
    fn bulk_ring(&self, dev: &Device, ep_address: u8) -> Result<usize, XhciError> {
        let dci = ctx::dci(ep_address);
        let k = dev
            .eps
            .iter()
            .position(|e| e.is_some_and(|(d, _)| d == dci && dci >= 2))
            .ok_or(XhciError::BadEndpoint)?;
        if !dev.bulk[k] {
            return Err(XhciError::BadEndpoint);
        }
        Ok(k)
    }

    fn bulk(&mut self, slot: u8, ep_address: u8, buf: u64, len: u32) -> Result<usize, XhciError> {
        // Checked again under the driver lock: a seal that came in after
        // the lock-free check in `bulk` is still honoured.
        if bulk_sealed() {
            return Err(XhciError::Sealed);
        }
        let (i, mut dev) = self.device(slot)?;
        let k = self.bulk_ring(&dev, ep_address)?;
        if dev.eps_broken[k] {
            return Err(XhciError::EndpointBroken);
        }
        let Some((dci, mut ring)) = dev.eps[k] else { return Err(XhciError::BadEndpoint) };
        let dma = self.idle()?;
        let ring_base = dev.page(PG_EPS + k as u64);
        let e = ring.enqueue(Trb::normal(buf, len, true));
        let at = ring_base + e.index as u64 * 16;
        // SAFETY: `e.index` and the link index are < TRANSFER_TRBS: TRB
        // slots of this endpoint's ring frame. The previous TD on the ring
        // completed (or the ring is broken and never rung again).
        unsafe {
            write_trb(at, e.trb);
            if let Some((li, link)) = e.link {
                write_trb(ring_base + li as u64 * 16, link);
            }
        }
        dev.eps[k] = Some((dci, ring));
        self.devices[i] = Some(dev);
        BULK_TDS.fetch_add(1, Ordering::AcqRel);
        // TRB visible before the doorbell reaches the controller.
        fence(Ordering::SeqCst);
        self.regs.write32(self.db + 4 * u64::from(slot), u32::from(dci))?;

        let want = Want::Transfer { slot, dci };
        let dl = Deadline::new(BULK_TIMEOUT_MS);
        let r = loop {
            let expired = dl.expired();
            if let Drained::Done(Event::Transfer { trb, code, residual, .. }) = self.drain_once(&dma, want)? {
                if trb != at {
                    // Names no TRB of this TD (hostile or stale): ignored.
                    self.queue.dropped = self.queue.dropped.saturating_add(1);
                } else if code == ring::SUCCESS || code == ring::SHORT_PACKET {
                    // A residual larger than the request is hostile: 0 bytes.
                    break Ok(len.saturating_sub(residual) as usize);
                } else {
                    break Err(XhciError::Transfer(code));
                }
            }
            if let Err(e) = self.check_running() {
                break Err(e);
            }
            if expired {
                break Err(XhciError::Timeout);
            }
            core::hint::spin_loop();
        };
        if r.is_err() {
            // Halted (stall) or unknown with the TD maybe still queued:
            // never enqueue behind it.
            if let Some(d) = self.devices[i].as_mut() {
                d.eps_broken[k] = true;
            }
        }
        r
    }

    /// One control TD on EP0 (Setup, optional Data, Status with IOC), then
    /// wait for the Status stage's transfer event. Exactly one TD is in
    /// flight on the ring (ledger Task 12); a short packet (code 13, ISP set
    /// on the IN data stage) is not an error.
    fn control(&mut self, slot: u8, packet: SetupPacket, data: Option<(u64, u32)>) -> Result<usize, XhciError> {
        let (i, mut dev) = self.device(slot)?;
        if dev.ep0_broken {
            return Err(XhciError::EndpointBroken);
        }
        let dma = self.idle()?;
        let dir_in = packet.dir_in();
        // Transfer Type: 0 no data, 2 OUT data, 3 IN data (table 6-26).
        let trt = match data {
            None => 0,
            Some(_) if dir_in => 3,
            Some(_) => 2,
        };
        let mut tds = [Trb::default(); 3];
        let mut n = 0;
        tds[n] = Trb::setup(packet.to_u64(), trt);
        n += 1;
        if let Some((buf, len)) = data {
            tds[n] = Trb::data(buf, len, dir_in);
            n += 1;
        }
        // Status stage in the opposite direction of the data (IN if none).
        tds[n] = Trb::status(!(data.is_some() && dir_in));
        n += 1;
        let ring_base = dev.page(PG_EP0);
        let mut at = [0u64; 3];
        for k in 0..n {
            let e = dev.ep0.enqueue(tds[k]);
            at[k] = ring_base + e.index as u64 * 16;
            // SAFETY: `e.index` and the link index are < TRANSFER_TRBS: TRB
            // slots of this device's EP0 ring frame. The previous TD on the
            // ring completed (or the ring is broken and not used).
            unsafe {
                write_trb(at[k], e.trb);
                if let Some((li, link)) = e.link {
                    write_trb(ring_base + li as u64 * 16, link);
                }
            }
        }
        self.devices[i] = Some(dev);
        let data_at = data.map(|_| at[1]);
        let status_at = at[n - 1];
        // TRBs visible before the doorbell reaches the controller.
        fence(Ordering::SeqCst);
        self.regs.write32(self.db + 4 * u64::from(slot), 1)?;

        let want = Want::Transfer { slot, dci: 1 };
        let len = data.map_or(0, |(_, l)| l);
        let mut residual = 0u32;
        let dl = Deadline::new(CONTROL_TIMEOUT_MS);
        let r = loop {
            // Every pass checks the deadline, even while (hostile) events
            // for this endpoint keep coming.
            let expired = dl.expired();
            if let Drained::Done(Event::Transfer { trb, code, residual: res, .. }) = self.drain_once(&dma, want)? {
                if !at[..n].contains(&trb) {
                    // Names no TRB of this TD (hostile or stale): ignored.
                    self.queue.dropped = self.queue.dropped.saturating_add(1);
                } else if code != ring::SUCCESS && code != ring::SHORT_PACKET {
                    break Err(XhciError::Transfer(code));
                } else if trb == status_at {
                    // A residual larger than the request is hostile: 0 bytes.
                    break Ok(len.saturating_sub(residual) as usize);
                } else if Some(trb) == data_at {
                    residual = res;
                }
            }
            if let Err(e) = self.check_running() {
                break Err(e);
            }
            if expired {
                break Err(XhciError::Timeout);
            }
            core::hint::spin_loop();
        };
        if r.is_err() {
            // Halted, or in an unknown state with the TD maybe still queued:
            // never enqueue behind it.
            if let Some(d) = self.devices[i].as_mut() {
                d.ep0_broken = true;
            }
        }
        r
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

    /// A hostile controller that never stops producing events: every drain
    /// pass returns after `EVENT_TRBS` events, so the caller's deadline and
    /// hardware checks still run.
    #[test_case]
    fn drain_is_bounded_with_endless_events() {
        let mut q = EventQueue::new();
        let mut stale = None;
        let mut produced = 0usize;
        let r: Result<Drained, ()> = drain(
            EVENT_TRBS,
            || {
                produced += 1;
                Ok(Some(Event::PortStatusChange { port: produced as u8 }))
            },
            |e| route(&mut q, &mut stale, e, Want::Command(0x1000)),
        );
        assert_eq!(r, Ok(Drained::Limit));
        assert_eq!(produced, EVENT_TRBS);
        assert_eq!(q.len, EVENT_QUEUE_LEN);
        assert_eq!(q.dropped, (EVENT_TRBS - EVENT_QUEUE_LEN) as u64);
        // Errors from the source propagate at once.
        let mut calls = 0;
        let e: Result<Drained, u8> = drain(EVENT_TRBS, || { calls += 1; Err(7) }, |_| None);
        assert_eq!((e, calls), (Err(7), 1));
    }

    /// Port events before the awaited completion are queued in order, a
    /// stray completion is counted, the stale completion clears `stale`.
    #[test_case]
    fn drain_routes_and_queues_events() {
        let script = [
            Event::PortStatusChange { port: 1 },
            Event::CommandCompletion { trb: 0x2000, code: 1, slot: 0 }, // stale
            Event::CommandCompletion { trb: 0xdead, code: 1, slot: 0 }, // stray
            Event::Transfer { trb: 0, code: 1, residual: 0, slot: 1, endpoint: 1 },
            Event::CommandCompletion { trb: 0x3000, code: 1, slot: 0 }, // awaited
            Event::PortStatusChange { port: 9 },
        ];
        let mut q = EventQueue::new();
        let mut stale = Some(0x2000);
        let mut i = 0;
        let mut next = || -> Result<Option<Event>, ()> {
            let e = script.get(i).copied();
            i += 1;
            Ok(e)
        };
        let r = drain(EVENT_TRBS, &mut next, |e| route(&mut q, &mut stale, e, Want::Command(0x3000)));
        assert_eq!(r, Ok(Drained::Done(script[4])));
        assert_eq!(stale, None);
        assert_eq!(q.dropped, 1);
        assert_eq!(q.pop(), Some(script[0]));
        assert_eq!(q.pop(), Some(script[3]));
        assert_eq!(q.pop(), None);
        // The rest stays for the next pass; then the source is empty.
        let r = drain(EVENT_TRBS, &mut next, |e| route(&mut q, &mut stale, e, Want::Nothing));
        assert_eq!(r, Ok(Drained::Empty));
        assert_eq!(q.pop(), Some(script[5]));
    }

    /// Transfer events go to a waiter for exactly that slot and endpoint;
    /// those of other endpoints are queued; a command waiter never takes them.
    #[test_case]
    fn transfer_events_route_by_slot_and_endpoint() {
        let mut q = EventQueue::new();
        let mut stale = None;
        let ev = |slot, endpoint| Event::Transfer { trb: 0x5000, code: 1, residual: 0, slot, endpoint };
        let want = Want::Transfer { slot: 2, dci: 1 };
        assert_eq!(route(&mut q, &mut stale, ev(2, 1), want), Some(ev(2, 1)));
        assert_eq!(route(&mut q, &mut stale, ev(2, 3), want), None);
        assert_eq!(route(&mut q, &mut stale, ev(1, 1), want), None);
        assert_eq!(route(&mut q, &mut stale, ev(2, 1), Want::Command(0x5000)), None);
        assert_eq!(route(&mut q, &mut stale, ev(2, 1), Want::Nothing), None);
        let cc = Event::CommandCompletion { trb: 0x5000, code: 1, slot: 2 };
        assert_eq!(route(&mut q, &mut stale, cc, want), None, "completion is no transfer");
        assert_eq!(q.dropped, 1);
        for e in [ev(2, 3), ev(1, 1), ev(2, 1), ev(2, 1)] {
            assert_eq!(q.pop(), Some(e));
        }
        assert_eq!(q.pop(), None);
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
        assert_eq!(generation(), None);
        assert_eq!(port_count(), 0);
        assert_eq!(portsc(1), Err(XhciError::NotInitialized));
        assert_eq!(portsc_unlocked(1), None);
        assert_eq!(prepare_port(1), Err(XhciError::NotInitialized));
        assert_eq!(slot_buffer(1), None);
        assert_eq!(slot_state(1), None);
    }
}
