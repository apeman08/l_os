#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

mod vga_buffer;
mod serial;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

// our panic handler in test mode
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    for i in 0..11 {
        println!("{}: Larp OS 0.0.2", i);
    }

    l_os::init();

    unsafe {
        *(0xdeadbeef as *mut u8) = 42;
    }
    
    #[cfg(test)]
    test_main();

    loop {}
}

/////////////////////////////////////////////////////////////////////////
/// TESTS
/////////////////////////////////////////////////////////////////////////

// Führt tests aus
#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    let mut word: &str = "tests";
    if tests.len() == 1 {
        word = "test";
    }
    serial_println!("Running {} {}", tests.len(), word);
    for test in tests {
        test.run();
    }

    exit_qemu(QemuExitCode::Success);
}


// Implementiert Debug Prints automatisch
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
        serial_println!("[ok]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}