//! Paging and physical frame management.
//!
//! The bootloader (feature `map_physical_memory`) maps all physical memory at
//! a fixed virtual offset; the kernel uses that to walk and edit page tables.

use crate::frames::{Cursor, Region};
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

static FRAMES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    cursor: Cursor,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The caller must guarantee that every region marked `Usable` in
    /// `memory_map` is really unused, and that the map outlives the allocator.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        Self { memory_map, cursor: Cursor::new() }
    }
}

// SAFETY: `Cursor` yields each usable frame at most once, so no frame is
// returned twice; usability is the contract of `init`.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let map = self.memory_map;
        let addr = self.cursor.next_frame(|i| {
            map.iter().nth(i).map(|r| Region {
                start: r.range.start_addr(),
                end: r.range.end_addr(),
                usable: r.region_type == MemoryRegionType::Usable,
            })
        })?;
        FRAMES_ALLOCATED.fetch_add(1, Ordering::Relaxed);
        Some(PhysFrame::containing_address(PhysAddr::new(addr)))
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
    let frames = unsafe { BootInfoFrameAllocator::init(memory_map) };
    *MEMORY.lock() = Some(Memory { mapper, frames });
}

pub fn frames_allocated() -> u64 {
    FRAMES_ALLOCATED.load(Ordering::Relaxed)
}

/// Total bytes in regions the bootloader reports as usable.
pub fn usable_bytes(memory_map: &MemoryMap) -> u64 {
    memory_map
        .iter()
        .filter(|r| r.region_type == MemoryRegionType::Usable)
        .map(|r| r.range.end_addr() - r.range.start_addr())
        .sum()
}
