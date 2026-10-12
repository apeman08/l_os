#![no_std]

#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::panic::PanicInfo;

pub mod serial;
pub mod vga_buffer;
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
    interrupts::idt_init();
    gdt::init();
    unsafe {interrupts::PICS.lock().initialize()};
    x86_64::instructions::interrupts::enable();
}

/// Set up paging access, the frame allocator and the kernel heap.
pub fn init_memory(boot_info: &'static bootloader::BootInfo) {
    use x86_64::VirtAddr;
    // SAFETY: values come straight from the bootloader's BootInfo, which was
    // built with `map_physical_memory`; this is called once during boot.
    unsafe {
        memory::init(VirtAddr::new(boot_info.physical_memory_offset), &boot_info.memory_map);
    }
    let mut guard = memory::MEMORY.lock();
    let mem = guard.as_mut().expect("memory initialized above");
    allocator::init_heap(mem).expect("kernel heap initialization failed");
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
        (Object::System, Rights::READ),
        (Object::Spawner((1u16 << PROGRAM_COUNT) - 1), Rights::USE),
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
bootloader::entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static bootloader::BootInfo) -> ! {
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