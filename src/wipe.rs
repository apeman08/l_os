//! Wipe plan (spec §8.3, §8.4): everything the wipe needs is computed once at
//! boot by [`arm`] and stored as fixed arrays in the page-aligned "island"
//! (`ISLAND`: wipe stack, plan, stage times). The wipe itself (Task 10) then
//! needs no heap, no lock and no firmware table.
//!
//! What the plan holds:
//! - `usable`: every `Usable` region of the boot memory map (stage 8 zeroes
//!   what is in it and not allocated; stages 6/7 use the bitmaps).
//! - `kernel_rw`: physical runs of the kernel's writable `PT_LOAD` segments
//!   (`.data`, `.bss`, without `PT_GNU_RELRO`) and of the boot kernel stack,
//!   minus the island (stage 9). These are `Bootloader` memory, never `Usable`
//!   (spike note §9), so stages 6–8 would miss them.
//! - `island_phys`: the island's physical pages (may be non-contiguous).
//! - `keys`, `fb`, `power`, `tsc_hz`, `kernel_l4`, `phys_offset`.
//!
//! Fail-closed: a list overflow, an unmapped page, a RELRO page shared with
//! writable data, or an island/kernel page inside a `Usable` region makes
//! [`arm`] fail; the plan then stays "not ready" and the status shows
//! "amnesisch: nein". No range is ever dropped silently.
//!
//! `ISLAND` is a `static mut` that only this module touches, and only through
//! raw pointers: [`arm`] writes the plan once while [`PLAN_READY`] is false;
//! afterwards the plan is only read, by the wipe in place (never copied).
//!
//! # The executor ([`trigger`], spec §8.2)
//!
//! The wipe runs in the context that detected the trigger (keyboard ISR, later
//! the timer ISR), with interrupts off, and never returns. It takes no lock,
//! allocates nothing and has no `panic!`/indexing/overflow path of its own
//! (all loops run over the fixed lists of the plan or the fixed bitmaps).
//! Stages, exactly in `l_core::wipe::ORDER`:
//!
//! 1. `InputLocked`: `cli`, wipe flag (in the island; ISRs drop input), all
//!    PIC lines masked and NMI disabled by port writes.
//! 2. `ScreenBlanked`: [`crate::framebuffer::blank_raw`].
//! 3. `DmaStopped`: bus-master bit of every enumerated PCI function cleared
//!    ([`crate::pci::disable_all_bus_masters_unlocked`]); the xHCI halt comes
//!    with the xHCI driver (Task 19).
//! 4. `StackSwitched`: one `asm!` loads CR3 with the kernel L4 and RSP with the
//!    top of the island stack and calls [`stage_rest`].
//! 5.–9. zero key slots, user frames, allocated frames, the rest of `Usable`,
//!    then the kernel's writable segments and boot stack (island excluded),
//!    all with `rep stos` through the physical-memory offset mapping.
//! 10. `Reported`: feature `wipe-test` only, the `WIPE-REPORT` line through
//!    [`crate::serial::raw_write`].
//! 11.–13. one `asm!` without stack ([`finish`]): island zeroed by its
//!    virtual address with `rep stosq`, `wbinvd`, S5 on PM1a/PM1b, wait, then
//!    `0xCF9` <- 6, `0x64` <- 0xFE, null IDT + `int3`, `cli; hlt`.
//!
//! From stage 9 on, only the island, registers, code, `.rodata` and RELRO are
//! used: everything the later stages need (trigger, stage times, power-off
//! ports, TSC frequency) is in the island.
//!
//! **Without a plan** (arming failed, "amnesisch: nein") the wipe still does
//! what it safely can: stages 1–3, the key slots (addresses from
//! [`secrets::slot_ranges`]) and the power-off/reset tail with the ACPI values
//! read lock-free ([`crate::acpi::power_off_plan_unlocked`]). Stages 4 and
//! 6–9 are skipped (passed without work): a plan fails exactly when zeroing
//! `Usable` or kernel memory could destroy what the wipe runs on, and without
//! the plan's lists there is nothing bounded to iterate.
//!
//! **Panic during a wipe:** [`on_panic`] (first thing in the kernel's panic
//! handler) sees the wipe flag in the island and jumps straight to
//! [`finish`] (power off / reset), without formatting and without touching a
//! lock or a `static` outside the island. After stage 9 the IDT is zero, so
//! any exception is a triple fault (reset) instead.
//!
//! In `cfg(test)` and with the feature `test-hooks`, [`trigger`] only counts
//! ([`test_hook_count`]) and returns; such builds never show "amnesisch: ja".

// Builds whose `trigger` only counts (cfg(test), feature `test-hooks`) do not
// use the real executor.
#![cfg_attr(any(test, feature = "test-hooks"), allow(dead_code))]

use crate::acpi::PowerOff;
use crate::secrets::{self, SLOTS};
use crate::{BootFacts, memory, println, time};
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use l_core::elf::{self, ElfError, LoadInfo};
use l_core::memrange::{PAGE, PhysRange, RangeList};
use l_core::wipe::{Amnesia, Stage, Trigger, WipeState};
use l_core::wipetime::StageTimes;

#[cfg(all(feature = "test-hooks", feature = "wipe-test"))]
compile_error!("features `test-hooks` and `wipe-test` exclude each other (a hooked build never wipes)");
use x86_64::VirtAddr;

/// Size of the wipe stack inside the island.
pub const ISLAND_STACK: usize = 16384;
/// Program headers are read only from this prefix of the kernel ELF.
const MAX_ELF_HEADER: u64 = 64 * 1024;

/// Everything the wipe needs, precomputed (spec §8.3). Plain `Copy` data with
/// fixed capacity: no pointer into the heap.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    /// All `Usable` regions of the boot memory map (byte-exact, merged).
    pub usable: RangeList<64>,
    /// Writable kernel segments without RELRO plus the boot kernel stack,
    /// minus the island; physical, page-granular.
    pub kernel_rw: RangeList<256>,
    /// Physical pages of the island.
    pub island_phys: RangeList<8>,
    /// Key slots: (virtual address, length).
    pub keys: [(u64, usize); SLOTS],
    /// Framebuffer: (virtual address, length in bytes); (0, 0) if none.
    pub fb: (u64, usize),
    /// ACPI S5 poweroff, if the tables provided one (otherwise reset only).
    pub power: Option<PowerOff>,
    /// TSC frequency in Hz from `time::calibrate_tsc` at boot; 0 means
    /// uncalibrated (the report then shows 0 µs).
    pub tsc_hz: u64,
    /// Physical address of the kernel's level-4 table (CR3 for stage 4).
    pub kernel_l4: u64,
    /// Virtual address at which all physical memory is mapped.
    pub phys_offset: u64,
}

impl Plan {
    const EMPTY: Plan = Plan {
        usable: RangeList::new(),
        kernel_rw: RangeList::new(),
        island_phys: RangeList::new(),
        keys: [(0, 0); SLOTS],
        fb: (0, 0),
        power: None,
        tsc_hz: 0,
        kernel_l4: 0,
        phys_offset: 0,
    };
}

/// Progress of a running wipe, kept in the island so that it survives stage 9.
#[derive(Clone, Copy, Debug)]
pub struct Exec {
    pub state: WipeState,
    pub times: StageTimes,
    /// Bytes written with zeros in stages 5–9 (`bytes=` of the report). User
    /// frames are written in stage 6 and again in stage 7, so they count
    /// twice; the island (stage 11) is not counted.
    pub bytes: u64,
    /// TSC frequency for the report (0 = uncalibrated).
    pub tsc_hz: u64,
    /// The full plan is used (false: degraded wipe without a plan).
    pub full: bool,
    /// Where stage 11 writes S5; `None`: reset fallback only.
    pub power: Option<PowerOff>,
}

impl Exec {
    const fn new() -> Exec {
        Exec { state: WipeState::new(), times: StageTimes::new(), bytes: 0, tsc_hz: 0, full: false, power: None }
    }

    /// Record that `stage` finished at TSC `at`. An order error cannot happen
    /// with the fixed sequence below and is ignored rather than panicking
    /// (the dry-run test checks the sequence).
    fn done(&mut self, stage: Stage, at: u64) {
        self.times.mark(stage, at);
        let _ = self.state.advance(stage);
    }

    fn add_bytes(&mut self, n: u64) {
        self.bytes = self.bytes.saturating_add(n);
    }
}

/// The memory the wipe runs on and from after stage 9: stack, plan, progress
/// and the wipe flag. Its pages are excluded from stage 9 and zeroed last
/// (stage 11).
#[repr(C, align(4096))]
struct Island {
    stack: [u8; ISLAND_STACK],
    plan: Plan,
    exec: Exec,
    /// 0: no wipe; 1: a wipe is running (set once by `trigger`).
    phase: AtomicU8,
}

static mut ISLAND: Island = Island {
    stack: [0; ISLAND_STACK],
    plan: Plan::EMPTY,
    exec: Exec::new(),
    phase: AtomicU8::new(0),
};

static ARMED_ONCE: AtomicBool = AtomicBool::new(false);
/// True once `ISLAND.plan` holds a complete, valid plan.
static PLAN_READY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmError {
    /// `arm` already ran (it is one-shot).
    AlreadyArmed,
    /// `memory::init` has not run.
    NoMemory,
    /// The kernel ELF header region is missing or implausibly large.
    KernelElfHeader,
    /// The kernel's program headers could not be read.
    KernelElf(ElfError),
    /// A virtual address computation overflowed.
    AddressOverflow,
    /// A RELRO page also holds writable non-RELRO data (it could be neither
    /// kept nor zeroed safely).
    RelroSharesPage(u64),
    /// A kernel page (virtual address) that must be planned is not mapped.
    Unmapped(u64),
    /// A kernel, island or page-table page (physical) lies in a `Usable`
    /// region: stages 7/8 would zero the code, data or page tables the wipe
    /// runs on.
    InUsable(u64),
    /// A writable kernel page is also a page of a read-only/executable
    /// segment: zeroing it in stage 9 would destroy code or `.rodata`.
    WritableSharesPage(u64),
    /// A fixed list of the plan overflowed (name of the list).
    ListFull(&'static str),
}

/// Compute the wipe plan and store it in the island. One-shot; called from
/// [`crate::init_memory`] after `acpi::init`. On error the plan stays not
/// ready (status "amnesisch: nein") and the reason is printed on the console.
pub fn arm(facts: &BootFacts) -> Result<(), ArmError> {
    if ARMED_ONCE.swap(true, Ordering::SeqCst) {
        return Err(ArmError::AlreadyArmed);
    }
    match build_plan(facts) {
        Ok(plan) => {
            // SAFETY: `ARMED_ONCE` makes this the only write to the plan, and
            // PLAN_READY is still false, so nothing reads it concurrently
            // (readers check PLAN_READY first). Only a raw pointer is formed;
            // no reference to the `static mut` exists anywhere.
            unsafe { (&raw mut ISLAND.plan).write(plan) };
            PLAN_READY.store(true, Ordering::Release);
            #[cfg(feature = "wipe-test")]
            wipe_test_boot_report();
            Ok(())
        }
        Err(e) => {
            PLAN_READY.store(false, Ordering::Release);
            println!("Wipe-Plan ungültig ({:?}): amnesisch: nein", e);
            #[cfg(feature = "wipe-test")]
            crate::serial_println!("wipe-test: arm failed ({:?})", e);
            Err(e)
        }
    }
}

/// A copy of the armed plan, `None` if [`arm`] did not succeed. For tests
/// and status only: the wipe reads the plan in place in the island
/// ([`island_plan`]), never copies it onto the 16 KiB island stack (it is
/// about 5.5 KiB).
pub fn plan() -> Option<Plan> {
    if !PLAN_READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: PLAN_READY is set only after the single write in `arm`
    // completed (Release/Acquire); afterwards the plan is never written
    // again by code that returns. Read through a raw pointer, by value.
    Some(unsafe { (&raw const ISLAND.plan).read() })
}

/// Virtual address and size in bytes of the island.
pub fn island_virt() -> (u64, usize) {
    (&raw const ISLAND as u64, core::mem::size_of::<Island>())
}

/// Status inputs for "amnesisch: ja/nein" (spec E7). There is no USB stick
/// driver yet, so `stick_sealed` is false. A build whose [`trigger`] only
/// counts (`test-hooks`, `cfg(test)`) reports `plan_ready = false`, so it can
/// never show "amnesisch: ja".
pub fn amnesia() -> Amnesia {
    Amnesia {
        stick_sealed: false,
        plan_ready: PLAN_READY.load(Ordering::Acquire) && !HOOKED,
        serial_mirror: crate::SERIAL_MIRROR,
    }
}

/// Read the program headers of the kernel ELF the bootloader loaded at
/// physical `facts.kernel_addr`. Only the header prefix is copied (its pages
/// are never written; `.data` pages of the same copy are, spike note §9).
pub fn kernel_load_info(facts: &BootFacts) -> Result<LoadInfo, ArmError> {
    let mut head = [0u8; 64];
    if facts.kernel_len < head.len() as u64 {
        return Err(ArmError::KernelElfHeader);
    }
    copy_from_phys(facts.kernel_addr, &mut head);
    let phoff = u64::from_le_bytes(head[32..40].try_into().unwrap());
    let phnum = u64::from(u16::from_le_bytes(head[56..58].try_into().unwrap()));
    let end = phnum
        .checked_mul(56)
        .and_then(|n| n.checked_add(phoff))
        .filter(|&e| e <= MAX_ELF_HEADER && e <= facts.kernel_len)
        .ok_or(ArmError::KernelElfHeader)?;
    let mut buf = vec![0u8; end.max(64) as usize];
    copy_from_phys(facts.kernel_addr, &mut buf);
    elf::load_segments(&buf).map_err(ArmError::KernelElf)
}

/// Copy `buf.len()` bytes from physical `addr` (inside the kernel ELF copy).
fn copy_from_phys(addr: u64, buf: &mut [u8]) {
    let src = (memory::phys_offset() + addr).as_ptr::<u8>();
    // SAFETY: callers pass a range inside [kernel_addr, kernel_addr +
    // kernel_len), the kernel ELF the bootloader loaded into `Bootloader`
    // memory (never handed out by the frame allocator) and mapped, like all
    // physical memory, at the offset. These header bytes are never written
    // after boot. `buf` is a distinct slice of exactly this length.
    unsafe { core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
}

const PTE_PRESENT: u64 = 1;
const PTE_HUGE: u64 = 1 << 7;
const PTE_ADDR: u64 = 0x000f_ffff_ffff_f000;

/// Result of walking the kernel's page tables for one virtual address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageWalk {
    /// Physical frames of the tables visited, L4 first; `levels` are valid.
    pub tables: [u64; 4],
    pub levels: usize,
    /// Physical address `virt` translates to.
    pub phys: u64,
    /// Size of the leaf mapping (4 KiB, 2 MiB or 1 GiB).
    pub leaf_size: u64,
}

/// Walk the kernel's own page tables (`memory::kernel_l4`) for `virt`;
/// `None` if not mapped or not canonical.
pub fn page_walk(virt: u64) -> Option<PageWalk> {
    VirtAddr::try_new(virt).ok()?;
    let off = memory::phys_offset().as_u64();
    let mut table = memory::kernel_l4().start_address().as_u64();
    let mut w = PageWalk { tables: [0; 4], levels: 0, phys: 0, leaf_size: 0 };
    for level in (1..=4u32).rev() {
        w.tables[w.levels] = table;
        w.levels += 1;
        let shift = 12 + 9 * (level - 1);
        let idx = (virt >> shift) & 0x1ff;
        let ptr = (off + table + idx * 8) as *const u64;
        // SAFETY: `table` is the kernel L4 or a frame a present non-leaf entry
        // points to, i.e. a page-table frame; all physical memory is mapped
        // at `off` (BOOTLOADER_CONFIG), and idx < 512 keeps the read inside
        // the 4 KiB table. Read by value; only boot code edits these tables.
        let e = unsafe { ptr.read_volatile() };
        if e & PTE_PRESENT == 0 {
            return None;
        }
        if level == 1 || (level <= 3 && e & PTE_HUGE != 0) {
            let size = 1u64 << shift;
            w.leaf_size = size;
            w.phys = (e & PTE_ADDR & !(size - 1)) + (virt & (size - 1));
            return Some(w);
        }
        table = e & PTE_ADDR;
    }
    None
}

/// Translate `virt` and require that neither its page-table frames (all
/// levels) nor its leaf frame lie in `usable`. Returns `(phys, leaf_size)`.
pub fn check_walk(virt: u64, usable: &RangeList<64>) -> Result<(u64, u64), ArmError> {
    let w = page_walk(virt).ok_or(ArmError::Unmapped(virt))?;
    for &t in &w.tables[..w.levels] {
        if usable.contains(t) {
            return Err(ArmError::InUsable(t));
        }
    }
    let leaf = w.phys & !(w.leaf_size - 1);
    if usable.contains(leaf) || usable.contains(w.phys) {
        return Err(ArmError::InUsable(w.phys));
    }
    Ok((w.phys, w.leaf_size))
}

/// Every translation of the physical-offset window that the wipe uses for
/// the physical range `r` must go through non-`Usable` tables (the leaf of
/// course is `r` itself, which may be `Usable`).
fn check_offset_window(r: PhysRange, usable: &RangeList<64>) -> Result<(), ArmError> {
    if r.is_empty() {
        return Ok(());
    }
    let off = memory::phys_offset().as_u64();
    let mut a = page_floor(r.start);
    while a < r.end {
        let virt = off.checked_add(a).ok_or(ArmError::AddressOverflow)?;
        let w = page_walk(virt).ok_or(ArmError::Unmapped(virt))?;
        for &t in &w.tables[..w.levels] {
            if usable.contains(t) {
                return Err(ArmError::InUsable(t));
            }
        }
        // Next leaf boundary.
        a = match (a | (w.leaf_size - 1)).checked_add(1) {
            Some(n) => n,
            None => return Ok(()),
        };
    }
    Ok(())
}

fn page_floor(a: u64) -> u64 {
    a & !(PAGE - 1)
}

/// Calls `f` with every page start of the virtual range `[start, end)`.
fn for_each_page(start: u64, end: u64, mut f: impl FnMut(u64) -> Result<(), ArmError>) -> Result<(), ArmError> {
    if end <= start {
        return Ok(());
    }
    let last = page_floor(end - 1);
    let mut p = page_floor(start);
    loop {
        f(p)?;
        if p == last {
            return Ok(());
        }
        p += PAGE;
    }
}

fn build_plan(facts: &BootFacts) -> Result<Plan, ArmError> {
    use bootloader_api::info::MemoryRegionKind;

    let mut plan = Plan::EMPTY;

    for r in facts.memory_regions.iter().filter(|r| r.kind == MemoryRegionKind::Usable) {
        plan.usable
            .push(PhysRange { start: r.start, end: r.end })
            .map_err(|_| ArmError::ListFull("usable"))?;
    }
    let usable = plan.usable;
    let not_usable = |phys: u64| if usable.contains(phys) { Err(ArmError::InUsable(phys)) } else { Ok(()) };

    let info = kernel_load_info(facts)?;
    let off = facts.kernel_image_offset;
    let reloc = |v: u64| v.checked_add(off).ok_or(ArmError::AddressOverflow);
    // RELRO, page-rounded outward: these pages are kept (spec §8.4).
    let relro_pages = match info.relro {
        Some((s, e)) if e > s => {
            let (s, e) = (reloc(s)?, reloc(e)?);
            let e = e.checked_next_multiple_of(PAGE).ok_or(ArmError::AddressOverflow)?;
            Some((page_floor(s), e))
        }
        _ => None,
    };
    let in_relro = |page: u64| relro_pages.is_some_and(|(s, e)| s <= page && page < e);

    // Held so that no one edits the kernel tables while they are walked.
    let guard = memory::MEMORY.lock();
    if guard.is_none() {
        return Err(ArmError::NoMemory);
    }
    // Every page the wipe runs on or reads: leaf AND all table frames must
    // stay intact through stages 7/8, i.e. must not be `Usable`.
    let translate = |virt: u64| check_walk(virt, &usable).map(|(p, _)| page_floor(p));

    // Island: its physical pages, never `Usable`.
    let (island, island_len) = island_virt();
    for_each_page(island, island + island_len as u64, |page| {
        let p = translate(page)?;
        not_usable(p)?;
        plan.island_phys
            .push(PhysRange { start: p, end: p + PAGE })
            .map_err(|_| ArmError::ListFull("island_phys"))
    })?;

    // Kernel segments: no page may be `Usable`; writable pages outside RELRO
    // are planned for stage 9.
    let mut rw = RangeList::<256>::new();
    let mut add_rw = |p: u64| {
        let end = p.checked_add(PAGE).ok_or(ArmError::AddressOverflow)?;
        rw.push(PhysRange { start: p, end }).map_err(|_| ArmError::ListFull("kernel_rw"))
    };
    for seg in info.segments() {
        let start = reloc(seg.vaddr)?;
        let end = start.checked_add(seg.mem_size).ok_or(ArmError::AddressOverflow)?;
        if seg.writable {
            // A writable page must not also be a page of a non-writable
            // segment (stage 9 would zero code or .rodata).
            for other in info.segments().filter(|o| !o.writable) {
                let os = reloc(other.vaddr)?;
                let oe = os.checked_add(other.mem_size).ok_or(ArmError::AddressOverflow)?;
                let (op0, op1) = (page_floor(os), page_floor(oe - 1));
                for page in [page_floor(start), page_floor(end - 1)] {
                    if op0 <= page && page <= op1 {
                        return Err(ArmError::WritableSharesPage(page));
                    }
                }
            }
            // A RELRO page must not also hold writable data outside RELRO.
            // The non-RELRO parts lie on either side of RELRO, so checking
            // their first and last page suffices.
            if let Some((s, e)) = info.relro {
                let (s, e) = (reloc(s)?, reloc(e)?);
                for (a, b) in [(start, end.min(s)), (start.max(e), end)] {
                    if a < b && (in_relro(page_floor(a)) || in_relro(page_floor(b - 1))) {
                        return Err(ArmError::RelroSharesPage(a));
                    }
                }
            }
        }
        for_each_page(start, end, |page| {
            let p = translate(page)?;
            not_usable(p)?;
            if seg.writable && !in_relro(page) { add_rw(p) } else { Ok(()) }
        })?;
    }
    // Boot kernel stack (we are running on it now).
    let stack_end = facts
        .kernel_stack_bottom
        .checked_add(facts.kernel_stack_len)
        .ok_or(ArmError::AddressOverflow)?;
    for_each_page(facts.kernel_stack_bottom, stack_end, |page| {
        let p = translate(page)?;
        not_usable(p)?;
        add_rw(p)
    })?;
    let island_ranges = plan.island_phys;
    let mut excl = [PhysRange { start: 0, end: 0 }; 8];
    for (slot, r) in excl.iter_mut().zip(island_ranges.iter()) {
        *slot = r;
    }
    plan.kernel_rw = rw.subtract(&excl[..island_ranges.len()]).map_err(|_| ArmError::ListFull("kernel_rw"))?;

    plan.kernel_l4 = memory::kernel_l4().start_address().as_u64();
    not_usable(plan.kernel_l4)?;
    plan.phys_offset = memory::phys_offset().as_u64();
    // The offset window through which stages 5-9 zero frames: its tables
    // must survive the zeroing of every range the wipe touches.
    for r in usable.iter().chain(plan.kernel_rw.iter()).chain(plan.island_phys.iter()) {
        check_offset_window(r, &usable)?;
    }
    drop(guard);

    plan.tsc_hz = time::tsc_hz();
    plan.keys = secrets::slot_ranges().map(|(v, n)| (v.as_u64(), n));
    plan.fb = crate::framebuffer::raw_region();
    plan.power = crate::acpi::power_off_plan();
    Ok(plan)
}

// ---------------------------------------------------------------------------
// The executor (Task 10). See the module documentation for the stage list.

/// Whether [`trigger`] only counts instead of wiping.
const HOOKED: bool = cfg!(any(test, feature = "test-hooks"));

/// The wipe flag in the island (it must survive stage 9, unlike any other
/// `static`).
fn phase() -> &'static AtomicU8 {
    // SAFETY: `ISLAND` is a static that lives for the whole run; `phase` is
    // an atomic, so shared references to it are fine. No `&mut` to the whole
    // island or to this field is ever created: `arm` writes only the `plan`
    // field and the wipe only the `exec` field through raw pointers; the
    // final `rep stosq` writes the island after its last Rust access.
    unsafe { &*(&raw const ISLAND.phase) }
}

/// True once a wipe has started (it never ends: the machine powers off).
/// ISRs drop input when this is set (stage `InputLocked`).
pub fn wiping() -> bool {
    phase().load(Ordering::SeqCst) != 0
}

/// The armed plan, in place in the island (never copied).
///
/// # Safety
/// [`PLAN_READY`] must have been observed `true` (Acquire): the single write
/// in [`arm`] is then complete and the plan is never written again.
unsafe fn island_plan() -> &'static Plan {
    // SAFETY: by the caller contract the plan is fully written and immutable
    // from now on; the reference points into a `static` that lives forever.
    unsafe { &*(&raw const ISLAND.plan) }
}

/// The wipe's progress record in the island.
///
/// # Safety
/// Only the wipe may call this, after it won the `phase` swap in [`trigger`]
/// (so exactly one wipe runs), with interrupts off on the single CPU, and it
/// must not keep two of these references alive at once.
unsafe fn island_exec() -> &'static mut Exec {
    // SAFETY: by the caller contract this is the only reference to `exec`;
    // no other code touches the field (it is not read outside the wipe).
    unsafe { &mut *(&raw mut ISLAND.exec) }
}

/// Write zeros over `[ptr, ptr + len)` with `rep stosq`/`rep stosb`. The
/// stores are inside `asm!`, so the compiler can neither elide nor reorder
/// them away (unlike `l_core::zeroize`'s best effort).
///
/// # Safety
/// The range must be mapped, writable memory that no live Rust reference
/// covers (or whose owners never run again: the wipe path).
pub(crate) unsafe fn zero_volatile(ptr: *mut u8, len: usize) {
    let qwords = len / 8;
    let rest = len % 8;
    // SAFETY: forwarded caller contract: [ptr, ptr + len) is writable. The
    // asm writes exactly `qwords * 8 + rest == len` bytes from `ptr` upward
    // (`cld` makes the direction explicit) and touches no stack.
    unsafe {
        core::arch::asm!(
            "cld",
            "rep stosq",
            "mov rcx, {rest}",
            "rep stosb",
            rest = in(reg) rest,
            inout("rcx") qwords => _,
            inout("rdi") ptr => _,
            in("rax") 0u64,
            options(nostack),
        );
    }
}

/// The work of each stage. [`Real`] does it; the dry-run test records it.
trait StageOps {
    fn now(&mut self) -> u64;
    fn lock_input(&mut self);
    fn blank_screen(&mut self);
    fn stop_dma(&mut self);
    /// Each `zero_*` returns the number of bytes it zeroed.
    fn zero_keys(&mut self) -> u64;
    fn zero_user(&mut self) -> u64;
    fn zero_allocated(&mut self) -> u64;
    fn zero_usable(&mut self) -> u64;
    fn zero_kernel(&mut self) -> u64;
    fn report(&mut self, line: &[u8]);
}

/// Stages 1–3 (on the interrupted context's stack and page tables).
fn run_front<O: StageOps>(o: &mut O, ex: &mut Exec) {
    o.lock_input();
    ex.done(Stage::InputLocked, o.now());
    o.blank_screen();
    ex.done(Stage::ScreenBlanked, o.now());
    o.stop_dma();
    ex.done(Stage::DmaStopped, o.now());
}

/// Stages 5–10 (on the island stack, kernel CR3, when a plan exists).
fn run_back<O: StageOps>(o: &mut O, ex: &mut Exec) {
    let n = o.zero_keys();
    ex.add_bytes(n);
    ex.done(Stage::KeysZeroed, o.now());
    let n = o.zero_user();
    ex.add_bytes(n);
    ex.done(Stage::UserZeroed, o.now());
    let n = o.zero_allocated();
    ex.add_bytes(n);
    ex.done(Stage::AllocatedZeroed, o.now());
    let n = o.zero_usable();
    ex.add_bytes(n);
    ex.done(Stage::UsableZeroed, o.now());
    let n = o.zero_kernel();
    ex.add_bytes(n);
    ex.done(Stage::KernelZeroed, o.now());
    // `total_us` in this line is the time from the trigger to `KernelZeroed`
    // only: a lower bound that leaves out this report, the island wipe and
    // the power-off itself (l_core::wipetime::report_line).
    if let Some(t) = ex.state.trigger_kind() {
        let mut line = [0u8; 192];
        let n = l_core::wipetime::report_line(&ex.times, t, ex.tsc_hz, ex.bytes, &mut line);
        o.report(line.get(..n).unwrap_or(&line));
    }
    ex.done(Stage::Reported, o.now());
}

/// Run every non-terminal stage against `o` without switching stack or page
/// tables, from a fresh record (not the island's): for tests.
#[cfg(test)]
fn dry_run<O: StageOps>(o: &mut O, t: Trigger) -> Exec {
    let mut ex = Exec::new();
    let _ = ex.state.trigger(t);
    ex.times.start(o.now());
    ex.tsc_hz = time::tsc_hz();
    run_front(o, &mut ex);
    ex.done(Stage::StackSwitched, o.now());
    run_back(o, &mut ex);
    ex
}

/// The real stage work. `plan` is `None` for the degraded wipe.
struct Real {
    plan: Option<&'static Plan>,
}

impl Real {
    /// Zero `len` bytes at physical `phys` through the offset mapping;
    /// returns the bytes zeroed (0 if the address would overflow).
    fn zero_phys(plan: &Plan, phys: u64, len: u64) -> u64 {
        let Some(virt) = plan.phys_offset.checked_add(phys) else { return 0 };
        if virt.checked_add(len).is_none() || len == 0 {
            return 0;
        }
        // SAFETY: `phys` lies in a range of the plan (a `Usable` region, a
        // frame of the allocator's or user bitmap, or a kernel `.data`/`.bss`
        // /boot-stack run). `arm` checked that all physical memory is mapped
        // at `phys_offset` through page tables that are not `Usable`, and that
        // none of these ranges holds the island, kernel code, `.rodata`,
        // RELRO or a page table the wipe uses. The wipe runs on the island
        // stack with the kernel CR3 and never returns, so no live reference
        // into this memory is used again.
        unsafe { zero_volatile(virt as *mut u8, len as usize) };
        len
    }

    /// Zero every frame whose bit is set in the bitmap `word(i)`.
    fn zero_bitmap(plan: &Plan, word: impl Fn(usize) -> u64) -> u64 {
        let mut bytes = 0u64;
        l_core::memrange::for_each_set_bit(memory::TRACKED_FRAMES_WORDS, word, |frame| {
            bytes = bytes.saturating_add(Real::zero_phys(plan, frame.saturating_mul(PAGE), PAGE));
        });
        bytes
    }
}

impl StageOps for Real {
    fn now(&mut self) -> u64 {
        time::rdtsc()
    }

    fn lock_input(&mut self) {
        use x86_64::instructions::port::PortWriteOnly;
        // SAFETY: 0x21/0xA1 are the data ports of the two 8259 PICs; writing
        // 0xFF masks every IRQ line (no lock: `PICS` is not used, and the
        // code that may hold it never runs again). 0x70 bit 7 disables NMI at
        // the chipset; register 0 is selected, nothing is written to CMOS.
        unsafe {
            PortWriteOnly::<u8>::new(0x21).write(0xFF);
            PortWriteOnly::<u8>::new(0xA1).write(0xFF);
            PortWriteOnly::<u8>::new(0x70).write(0x80);
        }
    }

    fn blank_screen(&mut self) {
        crate::framebuffer::blank_raw();
    }

    fn stop_dma(&mut self) {
        // Task 19 adds the xHCI halt (USBCMD.RS = 0) here.
        crate::pci::disable_all_bus_masters_unlocked();
    }

    fn zero_keys(&mut self) -> u64 {
        let keys = match self.plan {
            Some(p) => p.keys,
            None => secrets::slot_ranges().map(|(v, n)| (v.as_u64(), n)),
        };
        let mut bytes = 0u64;
        for (virt, len) in keys {
            if virt == 0 {
                continue;
            }
            // SAFETY: (virt, len) is one of the `secrets` key slots: a page-
            // aligned `static` in `.bss`, mapped in the kernel half of every
            // address space. Its users take `secrets::LOCK`; the wipe does
            // not, because whoever holds it never runs again (see
            // `secrets.rs`).
            unsafe { zero_volatile(virt as *mut u8, len) };
            bytes = bytes.saturating_add(len as u64);
        }
        bytes
    }

    fn zero_user(&mut self) -> u64 {
        match self.plan {
            Some(p) => Real::zero_bitmap(p, memory::user_word),
            None => 0,
        }
    }

    fn zero_allocated(&mut self) -> u64 {
        match self.plan {
            Some(p) => Real::zero_bitmap(p, memory::allocated_word),
            None => 0,
        }
    }

    fn zero_usable(&mut self) -> u64 {
        let Some(plan) = self.plan else { return 0 };
        let mut bytes = 0u64;
        let allocated = |frame: u64| {
            let w = memory::allocated_word((frame / 64) as usize);
            w >> (frame % 64) & 1 == 1
        };
        for r in plan.usable.iter() {
            let (head, inner, tail) = l_core::memrange::split_partial_pages(r);
            // Partial pages are never handed out (the allocator gives whole
            // frames inside a region), so they are zeroed unconditionally.
            for edge in [head, tail] {
                bytes = bytes.saturating_add(Real::zero_phys(plan, edge.start, edge.len()));
            }
            // Allocated frames were zeroed in stage 7.
            l_core::memrange::unallocated_runs(inner, memory::TRACKED_FRAMES, allocated, |run| {
                bytes = bytes.saturating_add(Real::zero_phys(plan, run.start, run.len()));
            });
        }
        bytes
    }

    fn zero_kernel(&mut self) -> u64 {
        let Some(plan) = self.plan else { return 0 };
        let mut bytes = 0u64;
        for r in plan.kernel_rw.iter() {
            bytes = bytes.saturating_add(Real::zero_phys(plan, r.start, r.len()));
        }
        // From here on: only the island, registers, code, .rodata, RELRO.
        bytes
    }

    fn report(&mut self, line: &[u8]) {
        if cfg!(feature = "wipe-test") {
            crate::serial::raw_write(line);
        }
    }
}

/// Start the wipe (spec §8). Called from interrupt context (the hotkey in the
/// keyboard ISR; later stick removal and controller loss in the timer ISR).
/// Never returns. If a wipe is already running (only reachable from an
/// exception inside the wipe), the caller is parked in `cli; hlt`.
#[cfg(not(any(test, feature = "test-hooks")))]
pub fn trigger(t: Trigger) -> ! {
    x86_64::instructions::interrupts::disable();
    if phase().swap(1, Ordering::SeqCst) != 0 {
        halt_forever();
    }
    let ready = PLAN_READY.load(Ordering::Acquire);
    // SAFETY: `ready` was observed with Acquire.
    let plan = if ready { Some(unsafe { island_plan() }) } else { None };
    let kernel_l4 = {
        // SAFETY: this call won the `phase` swap above, interrupts are off;
        // the reference ends with this block, before `stage_rest` creates
        // its own.
        let ex = unsafe { island_exec() };
        ex.times.start(time::rdtsc());
        let _ = ex.state.trigger(t);
        ex.full = ready;
        match plan {
            Some(p) => {
                ex.power = p.power;
                ex.tsc_hz = p.tsc_hz;
            }
            None => {
                ex.power = crate::acpi::power_off_plan_unlocked();
                ex.tsc_hz = time::tsc_hz();
            }
        }
        run_front(&mut Real { plan }, ex);
        plan.map(|p| p.kernel_l4)
    };
    match kernel_l4 {
        Some(l4) => switch_to_island(l4),
        // Degraded: no stack switch (nothing that the current stack lives in
        // gets zeroed).
        None => stage_rest(),
    }
}

/// Test builds: count instead of wiping (spec §5, feature `test-hooks`).
#[cfg(any(test, feature = "test-hooks"))]
pub fn trigger(_t: Trigger) {
    HOOK_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[cfg(any(test, feature = "test-hooks"))]
static HOOK_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// How often [`trigger`] was called (test builds only).
#[cfg(any(test, feature = "test-hooks"))]
pub fn test_hook_count() -> u64 {
    HOOK_COUNT.load(Ordering::SeqCst)
}

/// Stage 4: load the kernel's own L4 into CR3 (a process's tables are zeroed
/// in stages 6/7) and move RSP to the top of the island stack (the
/// interrupted stack may be a process kernel stack in the heap, zeroed in
/// stage 7), then continue in [`stage_rest`].
#[cfg(not(any(test, feature = "test-hooks")))]
fn switch_to_island(kernel_l4: u64) -> ! {
    // The stack is the first field of the 4096-aligned island, so its top is
    // 16-byte aligned; `call` pushes 8 bytes, which is the alignment the ABI
    // expects at function entry.
    let top = (&raw const ISLAND).addr() as u64 + ISLAND_STACK as u64;
    // SAFETY: `kernel_l4` is the L4 the kernel booted on (`arm` checked it is
    // not `Usable`); it maps the kernel's code, statics, island and the
    // physical-memory window exactly like every process L4 (which copies the
    // kernel slots), so the next instruction and all wipe data stay mapped.
    // `top` is the end of the island's 16 KiB stack, which nothing else uses
    // (the wipe runs once, interrupts are off). `stage_rest` never returns,
    // so abandoning the old stack (and this function's frame) is fine.
    unsafe {
        core::arch::asm!(
            "mov cr3, {l4}",
            "mov rsp, {top}",
            "xor ebp, ebp",
            "call {rest}",
            "ud2",
            l4 = in(reg) kernel_l4,
            top = in(reg) top,
            rest = sym stage_rest,
            options(noreturn),
        );
    }
}

/// Stages 4 (bookkeeping) to 13, on the island stack.
#[cfg(not(any(test, feature = "test-hooks")))]
extern "C" fn stage_rest() -> ! {
    // SAFETY: only reached from `trigger` after it won the `phase` swap and
    // dropped its own reference; interrupts are off.
    let ex = unsafe { island_exec() };
    // SAFETY: `full` is set only when PLAN_READY was observed true.
    let plan = if ex.full { Some(unsafe { island_plan() }) } else { None };
    ex.done(Stage::StackSwitched, time::rdtsc());
    run_back(&mut Real { plan }, ex);
    let power = ex.power;
    finish(power)
}

/// First call of the kernel's panic handler: during a wipe, a panic must not
/// format, lock or touch `static`s that may already be zero. It goes straight
/// to the terminal stages (island zeroed, power off, reset fallback). Returns
/// if no wipe is running.
pub fn on_panic() {
    if !HOOKED && wiping() {
        // SAFETY: a wipe is running and was interrupted by the panic; its
        // reference to `exec` is never used again. Only `power` is read.
        let power = unsafe { (&raw const ISLAND.exec.power).read() };
        finish(power);
    }
}

fn halt_forever() -> ! {
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// Pause iterations between power-off attempts (the S5 write takes effect at
/// once in QEMU; real hardware may need milliseconds).
const POWER_WAIT: u32 = 1 << 26;

/// Stages 11–13 in one `asm!` that uses only registers: zero the island
/// (including the stack this runs on and the plan) through its virtual
/// address with `rep stosq`, `wbinvd`, S5 through PM1a (and PM1b), wait;
/// then `0xCF9` <- 6, wait, `0x64` <- 0xFE, wait, a null IDT (read from the
/// now-zero island) and `int3` (triple fault), and finally `cli; hlt`.
fn finish(power: Option<PowerOff>) -> ! {
    let (base, len) = island_virt();
    let (have, pm1a, val_a, pm1b, val_b) = match power {
        Some(p) => (1u64, p.pm1a_cnt, p.value_a, p.pm1b_cnt, p.value_b),
        None => (0, 0, 0, 0, 0),
    };
    // SAFETY: [base, base + len) is the island, a mapped `static` whose size
    // is a multiple of 4096 (align 4096); nothing reads it afterwards (this
    // asm never returns and uses no stack, so overwriting the stack it was
    // called on is fine). The port writes are the ACPI PM1 control registers
    // from the FADT with the `_S5` value, then the standard reset ports. The
    // null IDT descriptor is read from the zeroed island start (limit 0), so
    // `int3` faults into a triple fault (reset). Interrupts are off.
    unsafe {
        core::arch::asm!(
            "cld",
            "rep stosq",
            "wbinvd",
            "test r12, r12",
            "jz 3f",
            // Stage 11: S5.
            "mov edx, r8d",
            "mov eax, r9d",
            "out dx, ax",
            "test r10d, r10d",
            "jz 2f",
            "mov edx, r10d",
            "mov eax, r11d",
            "out dx, ax",
            "2:",
            "mov ecx, {wait}",
            "21:",
            "pause",
            "dec ecx",
            "jnz 21b",
            // Stage 12: reset fallbacks.
            "3:",
            "mov dx, 0xcf9",
            "mov al, 6",
            "out dx, al",
            "mov ecx, {wait}",
            "31:",
            "pause",
            "dec ecx",
            "jnz 31b",
            "mov al, 0xfe",
            "out 0x64, al",
            "mov ecx, {wait}",
            "32:",
            "pause",
            "dec ecx",
            "jnz 32b",
            "lidt [rsi]",
            "int3",
            // Stage 13.
            "4:",
            "cli",
            "hlt",
            "jmp 4b",
            wait = const POWER_WAIT,
            in("rdi") base,
            in("rcx") len / 8,
            in("rax") 0u64,
            in("rsi") base,
            in("r12") have,
            in("r8") u64::from(pm1a),
            in("r9") u64::from(val_a),
            in("r10") u64::from(pm1b),
            in("r11") u64::from(val_b),
            options(noreturn, nostack),
        );
    }
}

/// Feature `wipe-test` (spec E12): fill key slot 0 from the hardware RNG and
/// tell the acceptance script (COM1, not the console) where to look.
#[cfg(feature = "wipe-test")]
fn wipe_test_boot_report() {
    use crate::serial_println;
    let filled = secrets::with_slot(0, |s| crate::rng::fill(s)).unwrap_or(false);
    if !filled {
        // Still non-zero, so that the check "zero after the wipe" means something.
        let _ = secrets::with_slot(0, |s| s.fill(0xA5));
        serial_println!("wipe-test: no hardware RNG, key slot 0 filled with 0xA5");
    }
    let (virt, len) = secrets::slot_ranges()[0];
    let phys = memory::virt_to_phys(virt).map_or(0, |p| p.as_u64());
    serial_println!("wipe-test: key-slot phys={:#x} len={}", phys, len);
    if let Some(plan) = plan() {
        for r in plan.usable.iter() {
            serial_println!("wipe-test: usable {:#x}-{:#x}", r.start, r.end);
        }
    }
    serial_println!("wipe-test: armed");
}

#[cfg(test)]
struct DryRun<'a> {
    buf: &'a mut [u8],
    calls: [Stage; 16],
    n: usize,
    line: [u8; 256],
    line_len: usize,
}

#[cfg(test)]
impl<'a> DryRun<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        DryRun { buf, calls: [Stage::Armed; 16], n: 0, line: [0; 256], line_len: 0 }
    }
    fn call(&mut self, s: Stage) {
        self.calls[self.n] = s;
        self.n += 1;
    }
}

#[cfg(test)]
impl StageOps for DryRun<'_> {
    fn now(&mut self) -> u64 {
        time::rdtsc()
    }
    fn lock_input(&mut self) {
        self.call(Stage::InputLocked);
    }
    fn blank_screen(&mut self) {
        self.call(Stage::ScreenBlanked);
    }
    fn stop_dma(&mut self) {
        self.call(Stage::DmaStopped);
    }
    fn zero_keys(&mut self) -> u64 {
        self.call(Stage::KeysZeroed);
        // SAFETY: `buf` is a `&mut [u8]` we own exclusively.
        unsafe { zero_volatile(self.buf.as_mut_ptr(), self.buf.len()) };
        self.buf.len() as u64
    }
    fn zero_user(&mut self) -> u64 {
        self.call(Stage::UserZeroed);
        0
    }
    fn zero_allocated(&mut self) -> u64 {
        self.call(Stage::AllocatedZeroed);
        0
    }
    fn zero_usable(&mut self) -> u64 {
        self.call(Stage::UsableZeroed);
        0
    }
    fn zero_kernel(&mut self) -> u64 {
        self.call(Stage::KernelZeroed);
        0
    }
    fn report(&mut self, line: &[u8]) {
        self.call(Stage::Reported);
        self.line[..line.len()].copy_from_slice(line);
        self.line_len = line.len();
    }
}

// ---------------------------------------------------------------------------
// In-crate tests (`cargo test --lib`): the non-terminal parts of the wipe.
// `trigger` only counts here (cfg(test)), so nothing is really wiped.

#[test_case]
fn feed_pushes_and_detects_hotkey_without_wiping_in_test() {
    use crate::input::{SCANCODES, feed};
    while SCANCODES.pop().is_some() {}
    let before = test_hook_count();
    for b in [0x1D, 0x38] {
        feed(b);
    }
    assert_eq!(test_hook_count(), before, "modifiers alone must not trigger");
    feed(0x0E);
    assert_eq!(test_hook_count(), before + 1, "Ctrl+Alt+Backspace triggers");
    // Release the modifiers again, so the detector is clean for others.
    feed(0x8E);
    feed(0x9D);
    feed(0xB8);
    feed(0x0E);
    assert_eq!(test_hook_count(), before + 1, "Backspace alone does not trigger");
    // Every byte reached the queue unchanged, in order.
    for b in [0x1D, 0x38, 0x0E, 0x8E, 0x9D, 0xB8, 0x0E] {
        assert_eq!(SCANCODES.pop(), Some(b));
    }
    assert!(SCANCODES.pop().is_none());
    assert!(!wiping());
}

#[test_case]
fn blank_raw_zeroes_framebuffer() {
    let (base, len) = crate::framebuffer::raw_region();
    assert!(base != 0 && len > 0, "test kernel has a framebuffer");
    let nonzero = crate::fb_console::with_framebuffer(|_, fb| fb.iter().any(|b| *b != 0));
    assert_eq!(nonzero, Some(true), "console painted something before");
    crate::framebuffer::blank_raw();
    let zero = crate::fb_console::with_framebuffer(|_, fb| fb.iter().all(|b| *b == 0));
    assert_eq!(zero, Some(true));
}

#[test_case]
fn disable_bus_master_clears_bit2() {
    use crate::pci;
    let nic = pci::find(0x1af4, 0x1000).expect("virtio-net in the test VM");
    pci::enable_bus_mastering(&nic);
    assert_ne!(pci::read32(nic.bus, nic.slot, nic.func, 0x04) & 0x4, 0);
    pci::disable_bus_master_unlocked(&nic);
    let cmd = pci::read32(nic.bus, nic.slot, nic.func, 0x04);
    assert_eq!(cmd & 0x4, 0, "bus master off");
    assert_eq!(cmd & 0x3, 0x3, "I/O and memory decoding untouched");
    // The same for every device through the lock-free list; afterwards every
    // command register is restored (status half written as 0: RW1C).
    let mut saved = [(0u8, 0u8, 0u8, 0u32); pci::MAX_DEVICES];
    let mut n = 0;
    for d in pci::devices() {
        saved[n] = (d.bus, d.slot, d.func, pci::read32(d.bus, d.slot, d.func, 0x04) & 0xffff);
        n += 1;
    }
    pci::enable_bus_mastering(&nic);
    pci::disable_all_bus_masters_unlocked();
    for &(b, s, f, _) in &saved[..n] {
        assert_eq!(pci::read32(b, s, f, 0x04) & 0x4, 0, "{:02x}:{:02x}.{} bus master off", b, s, f);
    }
    for &(b, s, f, cmd) in &saved[..n] {
        pci::write32(b, s, f, 0x04, cmd);
    }
    pci::enable_bus_mastering(&nic);
    assert_ne!(pci::read32(nic.bus, nic.slot, nic.func, 0x04) & 0x4, 0);
}

#[test_case]
fn raw_write_needs_no_lock() {
    let _guard = crate::serial::SERIAL1.lock();
    // Would deadlock if raw_write took the SERIAL1 lock.
    crate::serial::raw_write(b"(raw_write while SERIAL1 is held) ");
    drop(_guard);
}

#[test_case]
fn stage_sequence_of_dry_run() {
    use l_core::wipe::{Stage, Trigger};
    let mut buf = [0xA5u8; 4100];
    let mut dry = DryRun::new(&mut buf);
    let ex = dry_run(&mut dry, Trigger::Hotkey);
    assert_eq!(ex.state.stage(), Stage::Reported);
    assert_eq!(ex.state.trigger_kind(), Some(Trigger::Hotkey));
    assert_eq!(
        &dry.calls[..dry.n],
        &[
            Stage::InputLocked,
            Stage::ScreenBlanked,
            Stage::DmaStopped,
            Stage::KeysZeroed,
            Stage::UserZeroed,
            Stage::AllocatedZeroed,
            Stage::UsableZeroed,
            Stage::KernelZeroed,
            Stage::Reported,
        ]
    );
    assert!(dry.buf.iter().all(|b| *b == 0), "test buffer zeroed");
    assert_eq!(ex.bytes, 4100);
    for s in [Stage::KeysZeroed, Stage::UserZeroed, Stage::AllocatedZeroed, Stage::UsableZeroed, Stage::KernelZeroed] {
        assert!(ex.times.us_since_start(s, 1_000_000).is_some(), "{:?} marked", s);
    }
    let line = core::str::from_utf8(&dry.line[..dry.line_len]).unwrap();
    assert!(line.starts_with("WIPE-REPORT trigger=hotkey keys_us="), "{}", line);
    assert!(line.ends_with(" bytes=4100\n"), "{}", line);
    assert!(!wiping(), "a dry run does not set the global wipe state");
}
