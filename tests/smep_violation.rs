#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use l_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

bootloader::entry_point!(main);

fn main(boot_info: &'static bootloader::BootInfo) -> ! {
    use x86_64::{VirtAddr, structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags}};

    serial_print!("{}...\t", "smep_violation");
    l_os::init();
    l_os::init_memory(boot_info);
    assert!(l_os::cpu::smep_enabled(), "CPU/emulator does not provide SMEP (run QEMU with -cpu max)");
    let kernel_page = VirtAddr::new(0x_6666_0000_0000);
    let user_page = VirtAddr::new(0x_6666_0000_1000);
    {
        let mut guard = l_os::memory::MEMORY.lock();
        let mem = guard.as_mut().unwrap();
        let off = mem.mapper.phys_offset();
        for (virt, user) in [(kernel_page, false), (user_page, true)] {
            let frame = mem.frames.allocate_frame().unwrap();
            // SAFETY: fresh frame reached through the physical-memory mapping; 0xC3 = `ret`.
            unsafe { (off + frame.start_address().as_u64()).as_mut_ptr::<u8>().write(0xC3) };
            let mut flags = PageTableFlags::PRESENT;
            if user {
                flags |= PageTableFlags::USER_ACCESSIBLE;
            }
            // SAFETY: unused virtual addresses, fresh frames.
            unsafe { mem.mapper.map_to(Page::containing_address(virt), frame, flags, &mut mem.frames).unwrap().flush() };
        }
    }
    // Control: executing a supervisor page that contains `ret` works.
    // SAFETY: the page is mapped executable and holds a lone `ret`.
    unsafe { core::mem::transmute::<u64, extern "C" fn()>(kernel_page.as_u64())() };
    serial_print!("[control call ok] ");
    // Violation: executing a user-accessible page from ring 0 must fault under SMEP.
    // SAFETY: deliberately violating SMEP; expected to raise #PF before any byte runs.
    unsafe { core::mem::transmute::<u64, extern "C" fn()>(user_page.as_u64())() };
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
    if text.contains("PAGE FAULT") && text.contains("PROTECTION_VIOLATION") && text.contains("INSTRUCTION_FETCH") {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\nunexpected panic: {}", text);
        exit_qemu(QemuExitCode::Failed);
    }
    l_os::hlt_loop();
}
