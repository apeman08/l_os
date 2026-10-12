//! Kernel heap backed by `linked_list_allocator`.
//!
//! Rules: never allocate in an interrupt handler (the heap lock is a spin lock
//! and the interrupted code may hold it). The heap is fixed-size; exhaustion
//! returns an allocation error (panic via `alloc_error_handler` default).

use crate::memory::Memory;
use linked_list_allocator::LockedHeap;
use x86_64::{
    VirtAddr,
    structures::paging::{Mapper, Page, PageTableFlags, mapper::MapToError},
};

pub const HEAP_START: usize = 0x_4444_4444_0000;
pub const HEAP_SIZE: usize = 1024 * 1024;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_heap(mem: &mut Memory) -> Result<(), MapToError<x86_64::structures::paging::Size4KiB>> {
    use x86_64::structures::paging::FrameAllocator;

    let start = VirtAddr::new(HEAP_START as u64);
    let end = start + HEAP_SIZE as u64 - 1u64;
    let range = Page::range_inclusive(Page::containing_address(start), Page::containing_address(end));
    for page in range {
        let frame = mem.frames.allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        // SAFETY: the page is in a virtual range reserved for the heap and not
        // mapped elsewhere; the frame is freshly allocated and exclusively ours.
        unsafe { mem.mapper.map_to(page, frame, flags, &mut mem.frames)?.flush() };
    }
    // SAFETY: the range [HEAP_START, HEAP_START+HEAP_SIZE) was just mapped
    // writable and is used for nothing else; init is called once.
    unsafe { ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE) };
    Ok(())
}
