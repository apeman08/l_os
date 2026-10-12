#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use l_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

bootloader::entry_point!(main);

fn main(boot_info: &'static bootloader::BootInfo) -> ! {
    use x86_64::{VirtAddr, structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags}};

    serial_print!("{}...\t", "smap_violation");
    l_os::init();
    l_os::init_memory(boot_info);
    assert!(l_os::cpu::smap_enabled(), "CPU/emulator does not provide SMAP (run QEMU with -cpu max)");
    let virt = VirtAddr::new(0x_6666_0000_0000);
    {
        let mut guard = l_os::memory::MEMORY.lock();
        let mem = guard.as_mut().unwrap();
        let frame = mem.frames.allocate_frame().unwrap();
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        // SAFETY: unused virtual address, fresh frame.
        unsafe { mem.mapper.map_to(Page::containing_address(virt), frame, flags, &mut mem.frames).unwrap().flush() };
    }
    // Control: inside an explicit SMAP window the very same read works.
    // SAFETY: the page is mapped and readable.
    let v = unsafe { l_os::cpu::with_user_access(|| core::ptr::read_volatile(virt.as_ptr::<u8>())) };
    assert_eq!(v, 0);
    serial_print!("[control read ok] ");
    // Violation: the same read without the window must fault.
    // SAFETY: deliberately violating SMAP; expected to raise #PF.
    unsafe { core::ptr::read_volatile(virt.as_ptr::<u8>()) };
    serial_println!("[failed]\nno fault was raised");
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
    if text.contains("PAGE FAULT") && text.contains("PROTECTION_VIOLATION") {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\nunexpected panic: {}", text);
        exit_qemu(QemuExitCode::Failed);
    }
    l_os::hlt_loop();
}
