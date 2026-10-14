//! The kernel's own uncached MMIO window (spec E5): L4 slot 144
//! (`0x4800_0000_0000..0x4880_0000_0000`), 4 KiB pages with
//! `PRESENT | WRITABLE | NO_CACHE | WRITE_THROUGH | NO_EXECUTE`, supervisor
//! only. Device registers are never accessed through the bootloader's
//! physical-memory mapping (slot 32), which is cacheable (spike note §10).
//!
//! The slot's L3 table is created by [`init`] during [`crate::init_memory`],
//! before any process address space exists; [`crate::memory::create_address_space`]
//! copies the L4 entry, so later mappings (all below that shared L3) are
//! visible on every CR3, e.g. to the timer-ISR polling of the xHCI driver.
//! Mappings are handed out by a bump pointer and never removed; each is
//! followed by an unmapped guard page.
//!
//! **Wipe:** the window's page-table frames come from the frame allocator
//! (`Usable`), so wipe stage 7 zeroes them like any allocated frame; the
//! BAR pages themselves are device memory, never RAM, and no wipe stage
//! touches or depends on them ([`map`] refuses ranges overlapping
//! `Usable`/`Bootloader` RAM). The window is used only up to stage 3
//! (`DmaStopped`: the xHCI halt, `crate::xhci::halt_for_wipe`);
//! from stage 4 on the wipe runs on the kernel L4 and uses only the island,
//! code and the physical-memory mapping, whose tables are not `Usable`.

use crate::memory::{self, Memory};
use bootloader_api::info::MemoryRegionKind;
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, Page, PageTableFlags as F, PhysFrame, Size4KiB,
        mapper::MapToError, page_table::PageTableEntry,
    },
};

/// Level-4 slot of the window (spec E5). Free in the bootloader's layout:
/// physical-memory mapping from slot 32 (`PHYSICAL_MEMORY_OFFSET`), dynamic
/// mappings below slot 128 (`DYNAMIC_RANGE_END`), heap 136, user 224; [`init`]
/// asserts it is unused.
pub const MMIO_L4_INDEX: usize = 144;
/// First virtual address of the window.
pub const WINDOW_START: u64 = (MMIO_L4_INDEX as u64) << 39;
const WINDOW_END: u64 = WINDOW_START + (1 << 39);
/// Largest single mapping (bounds page-table frames spent on a hostile BAR
/// size; xHCI BARs are 64 KiB or less).
pub const MAX_MAP_LEN: u64 = 16 << 20;
const PAGE: u64 = 4096;

const _: () = assert!(MMIO_L4_INDEX != memory::USER_L4_INDEX);
const _: () = assert!(MMIO_L4_INDEX != crate::allocator::HEAP_START >> 39);
const _: () = assert!(MMIO_L4_INDEX as u64 > crate::DYNAMIC_RANGE_END >> 39);
const _: () = assert!(MMIO_L4_INDEX as u64 > crate::PHYSICAL_MEMORY_OFFSET >> 39);
const _: () = assert!(MMIO_L4_INDEX < 256);

/// Leaf flags of every window page.
pub const FLAGS: F = F::PRESENT
    .union(F::WRITABLE)
    .union(F::NO_CACHE)
    .union(F::WRITE_THROUGH)
    .union(F::NO_EXECUTE);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// [`init`] has not run (no memory manager / no window table).
    NotInitialized,
    /// `len == 0`.
    Empty,
    /// The range overflows or is not a valid physical address (52 bits).
    BadAddress,
    /// Longer than [`MAX_MAP_LEN`].
    TooLarge,
    /// The range overlaps RAM (`Usable` or `Bootloader`): mapping it
    /// uncached and writable would alias kernel or wipe memory.
    OverlapsRam,
    /// The window's 512 GiB are used up.
    WindowFull,
    /// No frame for a page table.
    OutOfFrames,
    /// A window page was already mapped or a table entry is a huge page
    /// (the window's tables were corrupted); nothing new stays mapped.
    PageTable,
}

/// Access to the registers of a device, `len` bytes at physical `phys`.
/// Every access is volatile, naturally aligned and bounds-checked.
#[derive(Debug)]
pub struct Mmio {
    virt: u64,
    phys: u64,
    len: u64,
}

/// An access outside the mapping or not naturally aligned; nothing was
/// accessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBounds {
    pub offset: u64,
}

/// Next free virtual address in the window; also marks [`init`] done.
static NEXT: Mutex<u64> = Mutex::new(0);

/// Create the L3 table of the window in the kernel L4. Must run before the
/// first process address space is created (they copy the kernel's L4
/// entries); called once by [`crate::init_memory`]. Panics if the slot is
/// already used (layout conflict) or on a second call.
pub fn init(mem: &mut Memory) {
    let mut next = NEXT.lock();
    assert_eq!(*next, 0, "mmio::init called twice");
    let frame = mem.frames.allocate_frame().expect("no frame for the MMIO window's L3 table");
    let l4 = mem.mapper.level_4_table_mut();
    assert!(
        l4[MMIO_L4_INDEX].is_unused(),
        "L4 slot {} (MMIO window) already used by the bootloader",
        MMIO_L4_INDEX
    );
    // The frame is zeroed by the allocator: an empty L3. No NO_EXECUTE here,
    // the leaves carry it; no USER_ACCESSIBLE anywhere.
    let mut e = PageTableEntry::new();
    e.set_frame(frame, F::PRESENT | F::WRITABLE);
    l4[MMIO_L4_INDEX] = e;
    *next = WINDOW_START;
}

fn overlaps_ram(start: u64, end: u64) -> Result<bool, MapError> {
    let facts = crate::boot_facts().ok_or(MapError::NotInitialized)?;
    Ok(facts.memory_regions.iter().any(|r| {
        matches!(r.kind, MemoryRegionKind::Usable | MemoryRegionKind::Bootloader) && r.start < end && start < r.end
    }))
}

/// Map `len` bytes of device memory at physical `phys` into the window
/// (whole pages, uncached, NX) and return an accessor for exactly that
/// range. Takes the [`memory::MEMORY`] lock; not for ISR context.
pub fn map(phys: u64, len: u64) -> Result<Mmio, MapError> {
    if len == 0 {
        return Err(MapError::Empty);
    }
    let end = phys.checked_add(len).ok_or(MapError::BadAddress)?;
    if PhysAddr::try_new(end - 1).is_err() {
        return Err(MapError::BadAddress);
    }
    if len > MAX_MAP_LEN {
        return Err(MapError::TooLarge);
    }
    if overlaps_ram(phys, end)? {
        return Err(MapError::OverlapsRam);
    }
    let first = phys & !(PAGE - 1);
    let pages = (end - first).div_ceil(PAGE);

    let mut guard = memory::MEMORY.lock();
    let mem = guard.as_mut().ok_or(MapError::NotInitialized)?;
    let mut next = NEXT.lock();
    if *next == 0 {
        return Err(MapError::NotInitialized);
    }
    let base = *next;
    // One extra unmapped guard page after the mapping.
    let new_next = base + (pages + 1) * PAGE;
    if new_next > WINDOW_END {
        return Err(MapError::WindowFull);
    }
    for i in 0..pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(base + i * PAGE));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(first + i * PAGE));
        // SAFETY: `page` lies in the window, which only this module maps and
        // whose addresses the bump pointer hands out once, so no reference to
        // memory at `page` exists. `frame` is device memory (checked above
        // not to overlap RAM the kernel or the allocator uses), mapped
        // uncached and non-executable, so the mapping aliases no Rust object.
        let res = unsafe { mem.mapper.map_to(page, frame, FLAGS, &mut mem.frames) };
        match res {
            Ok(flush) => flush.flush(),
            Err(e) => {
                // Roll back the pages mapped so far; the device frames are
                // not ours to free. Intermediate tables stay (empty, reused
                // by the next mapping at the same address).
                for j in 0..i {
                    let p = Page::<Size4KiB>::containing_address(VirtAddr::new(base + j * PAGE));
                    if let Ok((_, f)) = mem.mapper.unmap(p) {
                        f.flush();
                    }
                }
                return Err(match e {
                    MapToError::FrameAllocationFailed => MapError::OutOfFrames,
                    // The bump pointer never hands out a used page.
                    MapToError::PageAlreadyMapped(_) | MapToError::ParentEntryHugePage => MapError::PageTable,
                });
            }
        }
    }
    *next = new_next;
    Ok(Mmio { virt: base + (phys - first), phys, len })
}

impl Mmio {
    /// Virtual address of byte 0 (the mapped `phys`).
    pub fn virt(&self) -> u64 {
        self.virt
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Address of `size` bytes at `off`, if inside the mapping and naturally
    /// aligned (relative to the mapping start, which is how registers are
    /// laid out; BARs are at least 16-byte aligned).
    fn addr(&self, off: u64, size: u64) -> Result<u64, OutOfBounds> {
        let end = off.checked_add(size).ok_or(OutOfBounds { offset: off })?;
        if end > self.len || off % size != 0 {
            return Err(OutOfBounds { offset: off });
        }
        Ok(self.virt + off)
    }

    pub fn read32(&self, off: u64) -> Result<u32, OutOfBounds> {
        let a = self.addr(off, 4)?;
        // SAFETY: `a..a+4` lies inside this mapping (checked above), which
        // `map` created present, writable and uncached for the device's
        // registers and never unmaps; the address is 4-byte aligned. A
        // volatile read has no effect on Rust-visible memory.
        Ok(unsafe { core::ptr::read_volatile(a as *const u32) })
    }

    pub fn write32(&self, off: u64, v: u32) -> Result<(), OutOfBounds> {
        let a = self.addr(off, 4)?;
        // SAFETY: as in `read32`; the target is device memory, not a Rust
        // object, so the write aliases nothing the compiler knows about.
        unsafe { core::ptr::write_volatile(a as *mut u32, v) };
        Ok(())
    }

    /// 64-bit register write as two 32-bit writes, low dword first (the
    /// order xHCI §5.1 and Linux's `lo_hi_writeq` use). Both halves are
    /// checked before either is written.
    pub fn write64(&self, off: u64, v: u64) -> Result<(), OutOfBounds> {
        self.addr(off, 8)?;
        self.write32(off, v as u32)?;
        self.write32(off + 4, (v >> 32) as u32)
    }
}
