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

    // xHCI and the devices on its root ports (spec §7.1 steps 1–6); one
    // console line per device, nothing without a controller.
    #[cfg(not(test))]
    let _ = l_os::usb::enumerate();

    // Spec §7.1 steps 7–8: recognize the boot stick by the marker in its
    // LBA 0, then seal the storage path for good. Without a stick the
    // system keeps running and shows "amnesisch: nein" (E8).
    #[cfg(not(test))]
    {
        match l_os::usb::identify_boot_stick() {
            Ok(port) => l_os::println!("usb: boot-stick an port {} erkannt", port),
            Err(e) => l_os::println!("usb: kein boot-stick ({:?})", e),
        }
        l_os::usb::storage_seal();
        // Then the USB keyboard joins the input path (spec §7.2: the timer
        // ISR polls from here on).
        match l_os::usb::start_keyboard() {
            Ok(()) | Err(l_os::usb::UsbError::NoKeyboard) => {}
            Err(e) => l_os::println!("usb: tastatur nicht gestartet ({:?})", e),
        }
        // Spec §7.1 step 8: only an identified and sealed stick arms the
        // removal/controller-loss wipe (the real `wipe::trigger` in this
        // build); without one the system runs "amnesisch: nein" (E8).
        if let Err(e) = l_os::usb::arm_stick_watch() {
            if e != l_os::usb::WatchError::NoStick {
                l_os::println!("usb: abzieh-wipe nicht scharf ({:?})", e);
            }
        }
    }

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