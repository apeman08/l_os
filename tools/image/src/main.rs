//! `l_os_image <kernel-elf> <output.img>`: wrap a kernel ELF (built against
//! `bootloader_api` 0.11) into a bootable UEFI disk image.
//!
//! For scripts (scripts/qemu_*_smoke.py) that start QEMU themselves, so that
//! they use the same discovery as the cargo runner (`l_os_run`):
//! - `l_os_image --print-qemu`: path of qemu-system-x86_64
//! - `l_os_image --print-firmware`: the UEFI firmware file(s): one line for a
//!   combined image (used with `-bios`), two lines (code, then variable store)
//!   for a split pair (used as pflash)
//! - `l_os_image --print-qemu-firmware-args`: QEMU's firmware arguments, one
//!   per line (`-bios <file>`, or the two `-drive if=pflash,...`)
//! The overrides `OVMF_FD`, `OVMF_CODE` + `OVMF_VARS` apply to all of these.

use l_os_image::{Firmware, FirmwareEnv, HostOs, build_image, find_firmware, find_qemu, firmware_args};
use std::{env, path::PathBuf, process::ExitCode};

fn qemu() -> Result<PathBuf, String> {
    let dirs: Vec<PathBuf> = env::var_os("PATH").map(|p| env::split_paths(&p).collect()).unwrap_or_default();
    find_qemu(HostOs::current(), &dirs, &|p| p.is_file())
}

fn firmware() -> Result<Firmware, String> {
    let q = qemu().ok();
    find_firmware(HostOs::current(), &FirmwareEnv::from_process(), q.as_deref(), &|p| p.is_file())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--print-qemu") if args.len() == 2 => qemu().map(|q| println!("{}", q.display())),
        Some("--print-firmware") if args.len() == 2 => firmware().map(|f| match f {
            Firmware::Bios(p) => println!("{}", p.display()),
            Firmware::Pflash { code, vars } => println!("{}\n{}", code.display(), vars.display()),
        }),
        Some("--print-qemu-firmware-args") if args.len() == 2 => firmware().and_then(|f| firmware_args(&f)).map(|a| {
            for x in a {
                println!("{}", x.to_string_lossy());
            }
        }),
        Some(_) if args.len() == 3 => build_image(&PathBuf::from(&args[1]), &PathBuf::from(&args[2])),
        _ => {
            eprintln!(
                "usage: {0} <kernel-elf> <output.img>\n       {0} --print-qemu\n       {0} --print-firmware\n       {0} --print-qemu-firmware-args",
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
