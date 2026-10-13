//! `l_os_image <kernel-elf> <output.img>`: wrap a kernel ELF (built against
//! `bootloader_api` 0.11) into a bootable UEFI disk image.
//!
//! For scripts (scripts/qemu_*_smoke.py) that start QEMU themselves, so that
//! they use the same discovery as the cargo runner (`l_os_run`):
//! - `l_os_image --print-qemu`: path of qemu-system-x86_64
//! - `l_os_image --print-firmware`: path of the UEFI firmware for `-bios`
//!   (`$OVMF_FD` wins)

use l_os_image::{HostOs, build_image, find_firmware, find_qemu};
use std::{env, path::PathBuf, process::ExitCode};

fn qemu() -> Result<PathBuf, String> {
    let dirs: Vec<PathBuf> = env::var_os("PATH").map(|p| env::split_paths(&p).collect()).unwrap_or_default();
    find_qemu(HostOs::current(), &dirs, &|p| p.is_file())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--print-qemu") if args.len() == 2 => qemu().map(|q| println!("{}", q.display())),
        Some("--print-firmware") if args.len() == 2 => {
            let q = qemu().ok();
            find_firmware(HostOs::current(), env::var_os("OVMF_FD").as_deref(), q.as_deref(), &|p| p.is_file())
                .map(|f| println!("{}", f.display()))
        }
        Some(_) if args.len() == 3 => build_image(&PathBuf::from(&args[1]), &PathBuf::from(&args[2])),
        _ => {
            eprintln!(
                "usage: {0} <kernel-elf> <output.img>\n       {0} --print-qemu\n       {0} --print-firmware",
                args.first().map_or("l_os_image", String::as_str)
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("l_os_image: {e}");
            ExitCode::FAILURE
        }
    }
}
