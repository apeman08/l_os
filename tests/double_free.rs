#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use l_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

bootloader::entry_point!(main);

fn main(boot_info: &'static bootloader::BootInfo) -> ! {
    use x86_64::structures::paging::{FrameAllocator, FrameDeallocator};

    serial_print!("{}...\t", "double_free");
    l_os::init();
    l_os::init_memory(boot_info);
    let mut guard = l_os::memory::MEMORY.lock();
    let mem = guard.as_mut().unwrap();
    let f = mem.frames.allocate_frame().unwrap();
    // SAFETY: the frame is unused. The second call is the deliberate bug under test.
    unsafe {
        mem.frames.deallocate_frame(f);
        mem.frames.deallocate_frame(f);
    }
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
    if text.contains("not allocated (double free?)") {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\nunexpected panic: {}", text);
        exit_qemu(QemuExitCode::Failed);
    }
    l_os::hlt_loop();
}
