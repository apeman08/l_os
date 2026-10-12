//! Paging and physical frame management.
//!
//! The bootloader (feature `map_physical_memory`) maps all physical memory at
//! a fixed virtual offset; the kernel uses that to walk and edit page tables.

use crate::frames::{Cursor, FrameBitmap, Region};
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, FrameDeallocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

/// Frames above this number (4 GiB of physical memory) are never handed out.
const TRACKED_FRAMES_WORDS: usize = 16384; // 16384 * 64 = 1 Mi frames = 4 GiB
static ALLOCATED: Mutex<FrameBitmap<TRACKED_FRAMES_WORDS>> = Mutex::new(FrameBitmap::new());

static FRAMES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static FRAMES_FREED: AtomicU64 = AtomicU64::new(0);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
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
    pub unsafe fn init(memory_map: &'static MemoryMap, phys_offset: VirtAddr) -> Self {
        Self { memory_map, cursor: Cursor::new(), free_head: 0, phys_offset }
    }

    fn frame_ptr(&self, addr: u64) -> *mut u8 {
        (self.phys_offset + addr).as_mut_ptr::<u8>()
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
            let map = self.memory_map;
            self.cursor.next_frame(|i| {
                map.iter().nth(i).map(|r| Region {
                    start: r.range.start_addr(),
                    end: r.range.end_addr(),
                    usable: r.region_type == MemoryRegionType::Usable,
                })
            })?
        };
        if !ALLOCATED.lock().mark_allocated(addr / 4096) {
            // Beyond the tracked range (>4 GiB): refuse rather than lose track.
            // (A frame already marked allocated would be an allocator bug.)
            assert!(addr / 4096 >= FrameBitmap::<TRACKED_FRAMES_WORDS>::CAPACITY, "frame handed out twice");
            return None;
        }
        // SAFETY: the frame was just taken exclusively; stale data (including
        // the free-list pointer) must not leak to the next owner.
        unsafe { core::ptr::write_bytes(self.frame_ptr(addr), 0, 4096) };
        FRAMES_ALLOCATED.fetch_add(1, Ordering::Relaxed);
        Some(PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    /// # Safety (trait contract)
    /// The frame must no longer be mapped or referenced anywhere. Freeing a
    /// frame that is not currently allocated is detected and panics.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let addr = frame.start_address().as_u64();
        if !ALLOCATED.lock().mark_free(addr / 4096) {
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
pub unsafe fn init(physical_memory_offset: VirtAddr, memory_map: &'static MemoryMap) {
    use x86_64::registers::control::Cr3;

    if INITIALIZED.swap(true, Ordering::SeqCst) {
        panic!("memory::init called twice (would alias the level 4 page table)");
    }
    let (l4_frame, _) = Cr3::read();
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

pub fn frames_allocated() -> u64 {
    FRAMES_ALLOCATED.load(Ordering::Relaxed)
}

/// Frames currently handed out and not yet returned.
pub fn frames_in_use() -> u64 {
    FRAMES_ALLOCATED.load(Ordering::Relaxed) - FRAMES_FREED.load(Ordering::Relaxed)
}

/// Total bytes in regions the bootloader reports as usable.
pub fn usable_bytes(memory_map: &MemoryMap) -> u64 {
    memory_map
        .iter()
        .filter(|r| r.region_type == MemoryRegionType::Usable)
        .map(|r| r.range.end_addr() - r.range.start_addr())
        .sum()
}
