#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use l_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    serial_print!("{}...\t", "exception_invalid_opcode");
    l_os::init();
    // SAFETY: ud2 is the architecturally defined undefined instruction; it only raises #UD.
    unsafe { core::arch::asm!("ud2") };
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
    if text.contains("INVALID OPCODE") {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\nunexpected panic: {}", text);
        exit_qemu(QemuExitCode::Failed);
    }
    l_os::hlt_loop();
}
