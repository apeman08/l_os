//! Kernel-level tests for paging, frame allocation and the heap.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use x86_64::{
    VirtAddr,
    structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Translate},
};

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

#[test_case]
fn box_allocation() {
    let a = Box::new(41);
    let b = Box::new(1);
    assert_eq!(*a + *b, 42);
}

#[test_case]
fn large_vec() {
    let n = 1000u64;
    let mut v = Vec::new();
    for i in 0..n {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u64>(), (n - 1) * n / 2);
}

#[test_case]
fn memory_is_reused_after_free() {
    // Far more total allocation than the heap holds: only works if freed.
    for i in 0..(l_os::allocator::HEAP_SIZE) {
        let x = Box::new([i as u8; 256]);
        assert_eq!(x[0], i as u8);
    }
}

#[test_case]
fn map_write_read_unmap() {
    let mut guard = l_os::memory::MEMORY.lock();
    let mem = guard.as_mut().unwrap();
    let virt = VirtAddr::new(0x_5555_0000_0000);
    let page = Page::containing_address(virt);
    assert!(mem.mapper.translate_addr(virt).is_none());

    let frame = mem.frames.allocate_frame().unwrap();
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    // SAFETY: unused virtual address, fresh frame.
    unsafe { mem.mapper.map_to(page, frame, flags, &mut mem.frames).unwrap().flush() };
    assert_eq!(mem.mapper.translate_addr(virt), Some(frame.start_address()));

    // SAFETY: the page was just mapped writable.
    unsafe {
        (virt.as_mut_ptr::<u64>()).write_volatile(0xfeed_face);
        assert_eq!((virt.as_ptr::<u64>()).read_volatile(), 0xfeed_face);
    }

    let (_, flush) = mem.mapper.unmap(page).unwrap();
    flush.flush();
    assert!(mem.mapper.translate_addr(virt).is_none());
}

#[test_case]
fn frames_are_distinct() {
    let mut guard = l_os::memory::MEMORY.lock();
    let mem = guard.as_mut().unwrap();
    let a = mem.frames.allocate_frame().unwrap();
    let b = mem.frames.allocate_frame().unwrap();
    assert_ne!(a, b);
    assert!(a.start_address().as_u64() >= 0x1000);
}
