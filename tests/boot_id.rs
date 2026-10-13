#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(l_os::test_runner)]

use core::panic::PanicInfo;

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
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
    assert_ne!(a, [0u8; 16]);
}
