#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // During a wipe: no formatting, no locks; straight to power-off/reset.
    l_os::wipe::on_panic();
    // Serial first on purpose: it needs no framebuffer and no console state.
    l_os::serial::emergency_print(format_args!("KERNEL PANIC: {}\n", info));
    // Screen only (never blocks), so COM1 gets the message exactly once.
    l_os::fb_console::emergency_print_screen(format_args!("\nKERNEL PANIC: {}\n", info));
    l_os::hlt_loop();
}

// our panic handler in test mode
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

bootloader_api::entry_point!(kernel_main, config = &l_os::BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);

    #[cfg(test)]
    test_main();

    // Aurora chrome (status bar, hint bar) with its boot fade-in, before the
    // shell starts; opt-in, so test kernels keep the plain console.
    #[cfg(not(test))]
    l_os::aurora::show_chrome();

    #[cfg(not(test))]
    l_os::run_shell_forever();

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