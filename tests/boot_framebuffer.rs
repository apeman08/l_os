//! The kernel takes over the framebuffer the bootloader set up (UEFI GOP).
//! QEMU runs with `-vga none -device VGA,xres=1024,yres=768` (scripts/qemu_runner.sh).
//! Only the hand-over is checked here; drawing is tested in tests/framebuffer.rs.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_os::serial_println;

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
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
fn kernel_received_a_framebuffer() {
    let info = l_os::framebuffer::info().expect("the bootloader handed over no framebuffer");
    serial_println!(
        "  {}x{} {:?} {} bytes/pixel, stride {} px, {} bytes",
        info.width, info.height, info.pixel_format, info.bytes_per_pixel, info.stride, info.byte_len
    );
    assert!(info.width >= 1024, "width {}", info.width);
    assert!(info.height >= 768, "height {}", info.height);
    assert!(matches!(info.bytes_per_pixel, 3 | 4), "bytes per pixel {}", info.bytes_per_pixel);
    assert!(info.stride >= info.width, "stride {} < width {}", info.stride, info.width);
    assert!(info.byte_len >= info.stride * info.height * info.bytes_per_pixel, "buffer too small");
}

#[test_case]
fn framebuffer_memory_is_mapped() {
    let byte_len = l_os::framebuffer::info().expect("the bootloader handed over no framebuffer").byte_len;
    // The console owns the memory; read it through the console.
    l_os::fb_console::with_framebuffer(|_, buf| {
        assert_eq!(buf.len(), byte_len);
        // Reading the first and last byte faults if the mapping is missing.
        core::hint::black_box((buf[0], buf[buf.len() - 1]));
    })
    .expect("no framebuffer console");
}
