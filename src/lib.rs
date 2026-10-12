#![no_std]

#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::panic::PanicInfo;

pub mod serial;
pub mod fb_console;
pub mod framebuffer;
pub mod interrupts;
pub mod gdt;
pub mod frames;
pub mod memory;
pub mod net;
pub mod rng;
pub mod rtc;
pub mod pci;
pub mod virtio_net;
pub mod allocator;
pub mod input;
pub mod ipc;
pub mod sched;
pub mod syscall;
pub mod cpu;
pub mod time;
pub mod usermode;

pub fn init() {
    cpu::init();
    // Learn which devices exist (reads PCI configuration space; sends nothing).
    pci::enumerate();
    interrupts::idt_init();
    gdt::init();
    // SAFETY: remaps the PICs to the vectors the IDT (loaded above) handles,
    // then unmasks only lines with a handler. UEFI firmware leaves every PIC
    // line masked (the BIOS used to leave timer and keyboard on), so the lines
    // the kernel relies on are enabled explicitly: IRQ0 timer, IRQ1 keyboard,
    // IRQ2 cascade. Everything else stays masked, as it effectively was.
    unsafe {
        let mut pics = interrupts::PICS.lock();
        pics.initialize();
        pics.write_masks(0b1111_1000, 0b1111_1111);
    }
    x86_64::instructions::interrupts::enable();
}

/// Virtual address at which the bootloader maps all physical memory
/// (level-4 slot 32). Fixed so that it can never land on a slot the kernel
/// uses itself (heap: slot 136, user window: slot 224).
pub const PHYSICAL_MEMORY_OFFSET: u64 = 0x_1000_0000_0000;

/// Highest address the bootloader may pick for its dynamic mappings (kernel
/// stack, boot info, framebuffer): everything stays below level-4 slot 128,
/// i.e. below [`allocator::HEAP_START`] and the user slot
/// ([`memory::USER_L4_INDEX`]).
pub const DYNAMIC_RANGE_END: u64 = 0x_3fff_ffff_f000;

/// Boot configuration shared by the kernel and every test kernel; pass it to
/// `bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG)`.
pub const BOOTLOADER_CONFIG: bootloader_api::BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut config = bootloader_api::BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::FixedAddress(PHYSICAL_MEMORY_OFFSET));
    config.mappings.dynamic_range_end = Some(DYNAMIC_RANGE_END);
    // Same as bootloader 0.9's default (512 pages); 0.11 would give 80 KiB.
    config.kernel_stack_size = 512 * 4096;
    config
};

/// Take over what the bootloader handed over: the framebuffer, paging access,
/// the frame allocator and the kernel heap; then start the framebuffer console
/// (it needs the heap). Until then, console output goes only to the serial
/// mirror (feature `serial-console`).
pub fn init_memory(boot_info: &'static mut bootloader_api::BootInfo) {
    use x86_64::VirtAddr;
    let fb_memory = framebuffer::init(boot_info.framebuffer.take());
    let boot_info: &'static bootloader_api::BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory (BOOTLOADER_CONFIG not used?)");
    assert_eq!(offset, PHYSICAL_MEMORY_OFFSET, "physical memory mapped at an unexpected offset");
    // SAFETY: values come straight from the bootloader's BootInfo; the config
    // above makes it map all physical memory at `offset`, and the regions it
    // reports as usable are unused. This is called once during boot.
    unsafe {
        memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    }
    let mut guard = memory::MEMORY.lock();
    let mem = guard.as_mut().expect("memory initialized above");
    allocator::init_heap(mem).expect("kernel heap initialization failed");
    drop(guard);
    let fb_info = framebuffer::info().as_ref().and_then(framebuffer::render_info);
    if let (Some(info), Some(memory)) = (fb_info, fb_memory) {
        fb_console::init(info, memory);
    }
}

/// Run the interactive shell as a ring-3 process, forever. It holds exactly:
/// the console (write + grant), the keyboard, read-only system info, and a
/// spawner for the built-in programs. If it ends, it is started again.
pub fn run_shell_forever() -> ! {
    use l_core::handles::{Object, Rights};
    use usermode::{Spec, run_all, shell_program, PROGRAM_COUNT};

    let handles = [
        (Object::Console, Rights::WRITE.union(Rights::GRANT)),
        (Object::Keyboard, Rights::READ),
        (Object::System, Rights::READ.union(Rights::GRANT)),
        (Object::Spawner((1u16 << PROGRAM_COUNT) - 1), Rights::USE),
        // Trusted-path authority: only the shell may change network grants.
        (Object::NetManager, Rights::USE),
        // The right to ask; programs get it only if the shell passes it on.
        (Object::NetClient, Rights::USE.union(Rights::GRANT)),
        (Object::Random, Rights::READ.union(Rights::GRANT)),
    ];
    // A shell that dies right after starting would be restarted in a tight loop;
    // give up after a few very short runs in a row.
    const MIN_HEALTHY_TICKS: u64 = 18; // about one second
    const MAX_QUICK_FAILURES: u32 = 3;
    let mut quick_failures = 0;
    loop {
        let started = time::ticks();
        match run_all(&[Spec { program: shell_program(), handles: &handles, budget_ticks: u64::MAX, args: &[] }]) {
            Ok(results) => println!("\n[shell ended: {:?}; restarting]", results[0].outcome),
            Err(e) => {
                println!("\ncannot start the shell: {:?}", e);
                hlt_loop();
            }
        }
        if time::ticks() - started < MIN_HEALTHY_TICKS {
            quick_failures += 1;
            if quick_failures >= MAX_QUICK_FAILURES {
                println!("the shell keeps failing right after start; not restarting again");
                hlt_loop();
            }
        } else {
            quick_failures = 0;
        }
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial::emergency_print(format_args!("[failed]\n\nError: {}\n\n", info));
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

// Entry point for `cargo test`
#[cfg(test)]
bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    init();
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}