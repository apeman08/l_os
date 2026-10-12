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
pub mod allocator;
pub mod input;
pub mod shell;
pub mod sched;
pub mod syscall;
pub mod time;
pub mod usermode;

pub fn init() {
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