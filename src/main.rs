#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::println!("{}", info);
    l_os::hlt_loop();
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
        l_os::println!("{}: Larp OS 0.0.2", i);
    }

    l_os::init();

    use x86_64::registers::control::Cr3;

    let (level_4_page_table, _) = Cr3::read();
    l_os::println!("Level 4 page table at: {:?}", level_4_page_table.start_address());
    
    #[cfg(test)]
    test_main();

    l_os::hlt_loop();
}

/////////////////////////////////////////////////////////////////////////
/// TESTS
/////////////////////////////////////////////////////////////////////////

// Führt tests aus
#[cfg(test)]
pub fn test_runner(tests: &[&dyn l_os::Testable]) {
    let mut word: &str = "tests";
    if tests.len() == 1 {
        word = "test";
    }
    l_os::serial_println!("Running {} {}", tests.len(), word);
    for test in tests {
        test.run();
    }

    l_os::exit_qemu(l_os::QemuExitCode::Success);
}