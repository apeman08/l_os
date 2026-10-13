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
//! afterwards the plan is only read (and, in Task 10, by the wipe, which never
//! returns).

use crate::acpi::PowerOff;
use crate::secrets::{self, SLOTS};
use crate::{BootFacts, memory, println};
use alloc::vec;
use core::sync::atomic::{AtomicBool, Ordering};
use l_core::elf::{self, ElfError, LoadInfo};
use l_core::memrange::{PAGE, PhysRange, RangeList};
use l_core::wipe::Amnesia;
use l_core::wipetime::StageTimes;
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
    /// TSC frequency in Hz; 0 until TSC calibration exists (Task 10).
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

/// The memory the wipe runs on and from after stage 9: stack, plan and stage
/// times. Its pages are excluded from stage 9 and zeroed last (stage 11).
#[repr(C, align(4096))]
struct Island {
    stack: [u8; ISLAND_STACK],
    plan: Plan,
    times: StageTimes,
}

static mut ISLAND: Island = Island { stack: [0; ISLAND_STACK], plan: Plan::EMPTY, times: StageTimes::new() };

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
            Ok(())
        }
        Err(e) => {
            PLAN_READY.store(false, Ordering::Release);
            println!("Wipe-Plan ungültig ({:?}): amnesisch: nein", e);
            Err(e)
        }
    }
}

/// A copy of the armed plan, `None` if [`arm`] did not succeed. For tests
/// and status only: the wipe (Task 10) must read the plan in place in the
/// island (raw pointer to `ISLAND.plan`), never copy it onto the 16 KiB
/// island stack (it is about 5.5 KiB).
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
/// driver yet, so `stick_sealed` is false.
pub fn amnesia() -> Amnesia {
    Amnesia {
        stick_sealed: false,
        plan_ready: PLAN_READY.load(Ordering::Acquire),
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

    plan.keys = secrets::slot_ranges().map(|(v, n)| (v.as_u64(), n));
    plan.fb = crate::framebuffer::raw_region();
    plan.power = crate::acpi::power_off_plan();
    Ok(plan)
}
