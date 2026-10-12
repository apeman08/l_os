#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use l_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    serial_print!("{}...\t", "exception_gpf");
    l_os::init();
    // SAFETY: loading a selector beyond the GDT limit into DS raises #GP; DS is not used afterwards.
    unsafe { core::arch::asm!("mov ax, 0x1000", "mov ds, ax", out("ax") _) };
    serial_println!("[failed]\nno exception was raised");
    exit_qemu(QemuExitCode::Failed);
    l_os::hlt_loop();
}

struct Buf { b: [u8; 512], n: usize }
impl Write for Buf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let k = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
        self.n += k;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut buf = Buf { b: [0; 512], n: 0 };
    let _ = write!(buf, "{}", info);
    let text = core::str::from_utf8(&buf.b[..buf.n]).unwrap_or("");
    if text.contains("GENERAL PROTECTION FAULT") {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\nunexpected panic: {}", text);
        exit_qemu(QemuExitCode::Failed);
    }
    l_os::hlt_loop();
}
