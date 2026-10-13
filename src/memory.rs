//! Paging and physical frame management.
//!
//! The bootloader maps all physical memory at a fixed virtual offset
//! (`crate::BOOTLOADER_CONFIG`); the kernel uses that to walk and edit page tables.

use crate::frames::{Cursor, Region};
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, FrameDeallocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

/// Frames above this number (4 GiB of physical memory) are never handed out.
pub const TRACKED_FRAMES_WORDS: usize = 16384; // 16384 * 64 = 1 Mi frames = 4 GiB
/// Number of frames the bitmaps below can describe.
pub const TRACKED_FRAMES: u64 = TRACKED_FRAMES_WORDS as u64 * 64;

/// One bit per frame below 4 GiB: set while the frame is handed out. Atomic
/// words (not a lock) so the wipe can read them from any context
/// ([`allocated_word`]); the read-modify-write in [`bit_set`]/[`bit_clear`]
/// keeps the double-allocation and double-free checks exact without a lock.
static ALLOCATED: [AtomicU64; TRACKED_FRAMES_WORDS] = [const { AtomicU64::new(0) }; TRACKED_FRAMES_WORDS];

/// One bit per frame below 4 GiB: set while the frame is a leaf page of a
/// process's private user half (spec §8.3, wipe stage `UserZeroed`). Set when
/// a user page is mapped ([`mark_user_frame`]), cleared in
/// [`destroy_address_space`]. Frames above 4 GiB are never handed out, so the
/// bitmap covers every user frame.
pub static USER_FRAMES: [AtomicU64; TRACKED_FRAMES_WORDS] = [const { AtomicU64::new(0) }; TRACKED_FRAMES_WORDS];
static USER_FRAMES_MARKED: AtomicU64 = AtomicU64::new(0);

/// Set bit `frame` in `map`; `false` if out of range or already set.
fn bit_set(map: &[AtomicU64; TRACKED_FRAMES_WORDS], frame: u64) -> bool {
    if frame >= TRACKED_FRAMES {
        return false;
    }
    let bit = 1u64 << (frame % 64);
    map[(frame / 64) as usize].fetch_or(bit, Ordering::SeqCst) & bit == 0
}

/// Clear bit `frame` in `map`; `false` if out of range or it was not set.
fn bit_clear(map: &[AtomicU64; TRACKED_FRAMES_WORDS], frame: u64) -> bool {
    if frame >= TRACKED_FRAMES {
        return false;
    }
    let bit = 1u64 << (frame % 64);
    map[(frame / 64) as usize].fetch_and(!bit, Ordering::SeqCst) & bit != 0
}

fn bit_get(map: &[AtomicU64; TRACKED_FRAMES_WORDS], frame: u64) -> bool {
    frame < TRACKED_FRAMES && map[(frame / 64) as usize].load(Ordering::SeqCst) >> (frame % 64) & 1 == 1
}

/// Word `i` of the allocation bitmap (frames `64*i .. 64*i+63`, LSB first);
/// 0 for `i` out of range. Lock-free: callable from the wipe path.
pub fn allocated_word(i: usize) -> u64 {
    ALLOCATED.get(i).map_or(0, |w| w.load(Ordering::SeqCst))
}

/// Word `i` of the user-frame bitmap [`USER_FRAMES`] (same layout as
/// [`allocated_word`]); 0 for `i` out of range. Lock-free.
pub fn user_word(i: usize) -> u64 {
    USER_FRAMES.get(i).map_or(0, |w| w.load(Ordering::SeqCst))
}

/// Record that `frame` is now a user leaf page. Frames above 4 GiB are never
/// handed out by the allocator, so they never reach this.
pub fn mark_user_frame(frame: PhysFrame) {
    let n = frame.start_address().as_u64() / 4096;
    assert!(n < TRACKED_FRAMES, "user frame above the tracked range");
    let fresh = bit_set(&USER_FRAMES, n);
    debug_assert!(fresh, "user frame {:#x} marked twice", n * 4096);
    USER_FRAMES_MARKED.fetch_add(1, Ordering::Relaxed);
}

fn unmark_user_frame(frame: PhysFrame) {
    bit_clear(&USER_FRAMES, frame.start_address().as_u64() / 4096);
}

/// Whether frame number `n` (physical address / 4096) is a user leaf page.
pub fn is_user_frame(n: u64) -> bool {
    bit_get(&USER_FRAMES, n)
}

/// How many times a user frame was marked since boot (monotonic; for tests).
pub fn user_frames_marked() -> u64 {
    USER_FRAMES_MARKED.load(Ordering::Relaxed)
}

static FRAMES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static FRAMES_FREED: AtomicU64 = AtomicU64::new(0);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static KERNEL_L4: AtomicU64 = AtomicU64::new(0);
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Level-4 slot reserved for user mappings (virtual 0x7000_0000_0000..).
/// The kernel must never map anything inside it.
pub const USER_L4_INDEX: usize = 224;

pub struct BootInfoFrameAllocator {
    memory_map: &'static [MemoryRegion],
    cursor: Cursor,
    /// Physical address of the first free frame; each free frame stores the
    /// address of the next one in its first 8 bytes (0 = end of list).
    free_head: u64,
    phys_offset: VirtAddr,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The caller must guarantee that every region marked `Usable` in
    /// `memory_map` is really unused, that the map outlives the allocator, and
    /// that all physical memory is mapped at `phys_offset`.
    pub unsafe fn init(memory_map: &'static [MemoryRegion], phys_offset: VirtAddr) -> Self {
        Self { memory_map, cursor: Cursor::new(), free_head: 0, phys_offset }
    }

    fn frame_ptr(&self, addr: u64) -> *mut u8 {
        (self.phys_offset + addr).as_mut_ptr::<u8>()
    }
}

impl BootInfoFrameAllocator {
    /// Next frame straight from the boot memory map (never from the free list).
    fn take_fresh(&mut self) -> Option<u64> {
        let map = self.memory_map;
        self.cursor.next_frame(|i| {
            map.get(i).map(|r| Region { start: r.start, end: r.end, usable: r.kind == MemoryRegionKind::Usable })
        })
    }

    /// Mark `addr` allocated and hand out a zeroed frame; `None` if it lies
    /// beyond the tracked range (>4 GiB), which is refused rather than lost.
    fn claim(&mut self, addr: u64) -> Option<PhysFrame> {
        if !bit_set(&ALLOCATED, addr / 4096) {
            assert!(addr / 4096 >= TRACKED_FRAMES, "frame handed out twice");
            return None;
        }
        // SAFETY: the frame was just taken exclusively; stale data (including
        // the free-list pointer) must not leak to the next owner.
        unsafe { core::ptr::write_bytes(self.frame_ptr(addr), 0, 4096) };
        FRAMES_ALLOCATED.fetch_add(1, Ordering::Relaxed);
        Some(PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    /// Allocate `n` physically contiguous, zeroed frames (for DMA rings).
    /// Frames that do not fit a run are returned to the free list.
    pub fn allocate_contiguous(&mut self, n: usize) -> Option<PhysFrame> {
        use x86_64::structures::paging::FrameDeallocator as _;
        let mut run: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(n);
        while run.len() < n {
            let addr = self.take_fresh()?;
            if let Some(&last) = run.last() {
                if addr != last + 4096 {
                    // Break in contiguity: give the partial run back, restart.
                    for a in run.drain(..) {
                        let f = self.claim(a)?;
                        // SAFETY: claimed just above and never mapped anywhere.
                        unsafe { self.deallocate_frame(f) };
                    }
                }
            }
            run.push(addr);
        }
        let first = run[0];
        for a in run {
            self.claim(a)?;
        }
        Some(PhysFrame::containing_address(PhysAddr::new(first)))
    }
}

// SAFETY: a frame is returned only if it is marked free in the bitmap (or was
// never handed out by the cursor); marking it allocated happens before it is
// returned, so no frame is returned twice.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let addr = if self.free_head != 0 {
            let addr = self.free_head;
            // SAFETY: free frames are valid RAM mapped at phys_offset; the
            // first 8 bytes hold the next pointer we wrote in deallocate_frame.
            self.free_head = unsafe { (self.frame_ptr(addr) as *const u64).read() };
            addr
        } else {
            self.take_fresh()?
        };
        self.claim(addr)
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    /// # Safety (trait contract)
    /// The frame must no longer be mapped or referenced anywhere. Freeing a
    /// frame that is not currently allocated is detected and panics.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let addr = frame.start_address().as_u64();
        if !bit_clear(&ALLOCATED, addr / 4096) {
            panic!("free of frame {:#x} that is not allocated (double free?)", addr);
        }
        // SAFETY: caller guarantees nobody else uses the frame; it is valid RAM
        // mapped at phys_offset. Wipe it, then link it into the free list.
        unsafe {
            core::ptr::write_bytes(self.frame_ptr(addr), 0, 4096);
            (self.frame_ptr(addr) as *mut u64).write(self.free_head);
        }
        self.free_head = addr;
        FRAMES_FREED.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frames: BootInfoFrameAllocator,
}

/// Global memory manager, set once by [`init`].
pub static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// Initialize paging access and the frame allocator. Panics if called twice.
///
/// # Safety
/// `physical_memory_offset` must be the offset at which the bootloader mapped
/// all physical memory, and `memory_map` must be the bootloader's map.
pub unsafe fn init(physical_memory_offset: VirtAddr, memory_map: &'static [MemoryRegion]) {
    use x86_64::registers::control::Cr3;

    if INITIALIZED.swap(true, Ordering::SeqCst) {
        panic!("memory::init called twice (would alias the level 4 page table)");
    }
    let (l4_frame, _) = Cr3::read();
    KERNEL_L4.store(l4_frame.start_address().as_u64(), Ordering::SeqCst);
    PHYS_OFFSET.store(physical_memory_offset.as_u64(), Ordering::SeqCst);
    let virt = physical_memory_offset + l4_frame.start_address().as_u64();
    // SAFETY: the complete physical memory is mapped at `physical_memory_offset`
    // (caller contract) and the INITIALIZED guard ensures this is the only
    // `&mut` to the active level 4 table.
    let l4: &'static mut PageTable = unsafe { &mut *virt.as_mut_ptr::<PageTable>() };
    // SAFETY: same offset contract as above.
    let mapper = unsafe { OffsetPageTable::new(l4, physical_memory_offset) };
    // SAFETY: forwarded caller contract.
    let frames = unsafe { BootInfoFrameAllocator::init(memory_map, physical_memory_offset) };
    *MEMORY.lock() = Some(Memory { mapper, frames });
}

/// Remove what the bootloader left identity-mapped in the kernel's tables.
///
/// bootloader 0.11 identity-maps (virt == phys) the two pages of its
/// `context_switch` code and its GDT page, with flags PRESENT only, i.e.
/// supervisor-executable. The code pages lie in UEFI LOADER_CODE memory, which
/// the memory map reports as `Usable`; left in place, a frame handed to a
/// process would stay executable in ring 0 at a predictable address in every
/// address space ([`create_address_space`] copies all kernel L4 slots). The
/// bootloader reserves the L4 slots covering physical addresses
/// `[0, highest region end)` for exactly this identity map, so every such slot
/// is checked to hold nothing but identity mappings and then cleared. Their
/// table frames are bootloader memory (not `Usable`) and are simply left
/// unused. Must run before the first address space is created.
///
/// # Safety
/// The kernel's own GDT and IDT must already be loaded ([`crate::init`]), and
/// nothing may still use an identity-mapped address.
pub unsafe fn remove_boot_identity_map(mem: &mut Memory, memory_map: &[MemoryRegion]) {
    use x86_64::instructions::tables::{sgdt, sidt};
    use x86_64::structures::paging::PageTableFlags as F;

    let max_phys = memory_map.iter().map(|r| r.end).max().unwrap_or(0);
    let last_slot = (max_phys.saturating_sub(1) >> 39) as usize & 0x1ff;
    for (name, base) in [("GDT", sgdt().base), ("IDT", sidt().base)] {
        assert!(
            usize::from(base.p4_index()) > last_slot || base.as_u64() >= 1 << 47,
            "{} still in the bootloader's identity map ({:#x}); call crate::init first",
            name,
            base.as_u64()
        );
    }

    /// Every present leaf below `table` (level `level`, mapping virtual
    /// `base`) must map virt == phys.
    fn only_identity(table: &PageTable, level: u8, base: u64) -> bool {
        let shift = 12 + 9 * (level as u64 - 1);
        table.iter().enumerate().filter(|(_, e)| e.flags().contains(F::PRESENT)).all(|(i, e)| {
            let virt = base | ((i as u64) << shift);
            if level == 1 || e.flags().contains(F::HUGE_PAGE) {
                e.addr().as_u64() == virt
            } else {
                // SAFETY: a present non-leaf entry points at a page table,
                // which is RAM mapped at the physical-memory offset.
                let next = unsafe { &*table_ptr(PhysFrame::containing_address(e.addr())) };
                only_identity(next, level - 1, virt)
            }
        })
    }

    let l4 = mem.mapper.level_4_table_mut();
    for slot in 0..=last_slot {
        assert!(
            slot != USER_L4_INDEX && slot < 256,
            "bootloader identity map would reach L4 slot {}",
            slot
        );
        if l4[slot].is_unused() {
            continue;
        }
        // SAFETY: as above, the slot's L3 table is RAM at the offset.
        let l3 = unsafe { &*table_ptr(PhysFrame::containing_address(l4[slot].addr())) };
        assert!(
            only_identity(l3, 3, (slot as u64) << 39),
            "L4 slot {} holds more than the bootloader's identity map",
            slot
        );
        l4[slot].set_unused();
    }
    // The removed entries were not GLOBAL, so reloading CR3 drops them all.
    x86_64::instructions::tlb::flush_all();
}

pub fn frames_allocated() -> u64 {
    FRAMES_ALLOCATED.load(Ordering::Relaxed)
}

/// Frames currently handed out and not yet returned.
pub fn frames_in_use() -> u64 {
    FRAMES_ALLOCATED.load(Ordering::Relaxed) - FRAMES_FREED.load(Ordering::Relaxed)
}

/// Total bytes in regions the bootloader reports as usable.
pub fn usable_bytes(memory_map: &[MemoryRegion]) -> u64 {
    memory_map
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable)
        .map(|r| r.end - r.start)
        .sum()
}

/// The level-4 table the kernel itself runs on.
pub fn kernel_l4() -> PhysFrame {
    PhysFrame::containing_address(PhysAddr::new(KERNEL_L4.load(Ordering::SeqCst)))
}

pub fn phys_offset() -> VirtAddr {
    VirtAddr::new(PHYS_OFFSET.load(Ordering::SeqCst))
}

/// Physical address behind `virt` in the kernel's own page tables, or `None`
/// if unmapped or memory is not initialized. Takes the [`MEMORY`] lock.
pub fn virt_to_phys(virt: VirtAddr) -> Option<PhysAddr> {
    use x86_64::structures::paging::Translate;
    MEMORY.lock().as_ref()?.mapper.translate_addr(virt)
}

fn table_ptr(frame: PhysFrame) -> *mut PageTable {
    (phys_offset() + frame.start_address().as_u64()).as_mut_ptr::<PageTable>()
}

#[derive(Debug, PartialEq, Eq)]
pub struct OutOfFrames;

/// Create a fresh address space: a new level-4 table that shares every kernel
/// mapping (copied entries; all kernel pages are supervisor-only) and has an
/// empty, private user slot.
pub fn create_address_space(mem: &mut Memory) -> Result<PhysFrame, OutOfFrames> {
    use x86_64::structures::paging::page_table::PageTableEntry;

    let new = mem.frames.allocate_frame().ok_or(OutOfFrames)?;
    // SAFETY: both frames are valid page-table frames mapped at the
    // physical-memory offset; the new one is exclusively ours and zeroed, the
    // kernel table is only read.
    unsafe {
        let src = &*table_ptr(kernel_l4());
        let dst = &mut *table_ptr(new);
        for i in 0..512 {
            dst[i] = src[i].clone();
        }
        assert!(
            dst[USER_L4_INDEX].is_unused(),
            "kernel mapped something in the user L4 slot"
        );
        dst[USER_L4_INDEX] = PageTableEntry::new();
    }
    Ok(new)
}

/// Build a mapper that edits the page tables rooted at `l4`.
///
/// # Safety
/// `l4` must be a table created by [`create_address_space`], and no other
/// reference to it may be live while the mapper is used.
pub unsafe fn mapper_for(l4: PhysFrame) -> OffsetPageTable<'static> {
    // SAFETY: forwarded caller contract; offset is the bootloader's mapping.
    unsafe { OffsetPageTable::new(&mut *table_ptr(l4), phys_offset()) }
}

/// Free every frame of the private user half (leaf frames and the table
/// frames above them), then the level-4 table itself. Must not be called
/// while `l4` is the active CR3 or while the process can still run.
pub fn destroy_address_space(mem: &mut Memory, l4: PhysFrame) {
    use x86_64::structures::paging::FrameDeallocator;

    // SAFETY: table frames are valid and mapped at the physical offset. Entries
    // are read before their frames are freed; the address space is dead, so no
    // one else references these frames.
    unsafe {
        let l4t = &mut *table_ptr(l4);
        if !l4t[USER_L4_INDEX].is_unused() {
            let l3f = l4t[USER_L4_INDEX].frame().expect("user L4 entry is not a plain table");
            let l3 = &*table_ptr(l3f);
            for e3 in l3.iter().filter(|e| !e.is_unused()) {
                let l2f = e3.frame().expect("huge page in user space");
                let l2 = &*table_ptr(l2f);
                for e2 in l2.iter().filter(|e| !e.is_unused()) {
                    let l1f = e2.frame().expect("huge page in user space");
                    let l1 = &*table_ptr(l1f);
                    for e1 in l1.iter().filter(|e| !e.is_unused()) {
                        let leaf = e1.frame().expect("bad user leaf");
                        unmark_user_frame(leaf);
                        mem.frames.deallocate_frame(leaf);
                    }
                    mem.frames.deallocate_frame(l1f);
                }
                mem.frames.deallocate_frame(l2f);
            }
            mem.frames.deallocate_frame(l3f);
        }
        mem.frames.deallocate_frame(l4);
    }
}

/// Run `f` with a read-only view of the *currently active* page tables.
/// Only for use with interrupts disabled (syscall/ISR context).
pub fn with_active_tables<R>(f: impl FnOnce(&OffsetPageTable) -> R) -> Option<R> {
    use x86_64::registers::control::Cr3;

    let (active, _) = Cr3::read();
    if active == kernel_l4() {
        return None; // syscalls only make sense on a process's tables
    }
    // SAFETY: interrupts are off, so nothing else touches the tables; a
    // process's tables are never referenced by another live `&mut` while that
    // process is running (builders and destroyers run only for non-running
    // address spaces).
    let mapper = unsafe { OffsetPageTable::new(&mut *table_ptr(active), phys_offset()) };
    Some(f(&mapper))
}
