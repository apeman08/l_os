#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(l_os::test_runner)]

use core::panic::PanicInfo;

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

// The image builder patched this very kernel before the image was made.
// (Do not mention the placeholder constant here: a second copy in the binary
// would make the builder refuse the kernel.)
#[test_case]
fn kernel_carries_embedded_boot_id() {
    let a = l_os::boot_id().expect("kernel was not patched by l_os_image");
    assert_eq!(l_os::boot_id(), Some(a));
}

// Recomputing the hash from the loaded ELF is not possible: the kernel file
// the bootloader leaves in RAM is not pristine (relocation and writable
// segments are modified in place; measured: 3 of 175 64-KiB blocks differ),
// so the end-to-end id chain (LBA0 == embedded == compute_id of the unpatched
// kernel) is covered by the host test
// `image_chain_lba0_embedded_and_computed_id_agree` in tools/image.
// Here: the id is a real hash, and exactly the marker the builder wrote is
// present once in the loaded kernel file.
#[test_case]
fn embedded_id_is_the_marker_in_the_loaded_kernel() {
    use l_core::bootmark::embedded;
    let id = l_os::boot_id().expect("kernel was not patched by l_os_image");
    let mut distinct = [false; 256];
    for &b in &id {
        distinct[b as usize] = true;
    }
    assert!(distinct.iter().filter(|&&d| d).count() >= 8, "id {:?} does not look like a hash", id);
    let facts = l_os::boot_facts().expect("boot facts");
    let base = (l_os::PHYSICAL_MEMORY_OFFSET + facts.kernel_addr) as *const u8;
    // SAFETY: the bootloader loaded the kernel ELF file at `kernel_addr`
    // (`kernel_len` bytes); it stays in RAM and all physical memory is mapped
    // at PHYSICAL_MEMORY_OFFSET; nothing writes to it.
    let file = unsafe { core::slice::from_raw_parts(base, facts.kernel_len as usize) };
    let marker = embedded(&id);
    assert_eq!(file.windows(32).filter(|w| *w == marker).count(), 1);
}
