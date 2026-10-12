//! `l_os_image <kernel-elf> <output.img>`: wrap a kernel ELF (built against
//! `bootloader_api` 0.11) into a bootable UEFI disk image.
//!
//! No minimum framebuffer size is configured on purpose: with one, the UEFI
//! loader picks the *largest* GOP mode (2560x1600 under OVMF). Without it, the
//! current mode is kept, which QEMU sets via `-device VGA,xres=..,yres=..`.

use bootloader::{BootConfig, UefiBoot};
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <kernel-elf> <output.img>", args.first().map_or("l_os_image", String::as_str));
        return ExitCode::from(2);
    }
    let kernel = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);

    let mut config = BootConfig::default();
    // The kernel owns COM1: test output and the console mirror go there, and
    // the bootloader's log lines would mix into it. L_OS_BOOT_LOG=1 turns them
    // on for debugging boot problems (e.g. a bootloader panic before the kernel).
    config.serial_logging = std::env::var_os("L_OS_BOOT_LOG").is_some_and(|v| v == "1");

    match UefiBoot::new(&kernel).set_boot_config(&config).create_disk_image(&out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("l_os_image: cannot build {} from {}: {e:#}", out.display(), kernel.display());
            ExitCode::FAILURE
        }
    }
}
