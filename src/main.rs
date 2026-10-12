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

bootloader::entry_point!(kernel_main);

fn kernel_main(boot_info: &'static bootloader::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);

    l_os::println!("Larp OS {}", env!("CARGO_PKG_VERSION"));

    #[cfg(test)]
    test_main();

    #[cfg(not(test))]
    l_os::shell::run();

    #[cfg(test)]
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