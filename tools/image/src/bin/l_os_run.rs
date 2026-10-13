//! `l_os_run <kernel-elf> [args...]`: the cargo runner for the kernel target
//! (`.cargo/config.toml`). It replaces `bootimage runner` and the former bash
//! runner, so `cargo run` / `cargo test` work without a shell (also on Windows).
//!
//! - Test binaries (file name ends in `-<16 hex digits>`, optionally `.exe`):
//!   boot headless, serial on stdout, exit via isa-debug-exit. QEMU exit code
//!   33 (the kernel wrote 0x10) means success -> 0; anything else, including
//!   the 300 s timeout -> 1. QEMU's packet capture of the NIC goes to
//!   `<root>/target/net-trace.pcap` (scripts/net_tests.sh inspects it).
//! - Anything else (`cargo run`): boot the kernel with a window, serial on
//!   stdout; QEMU's exit status is passed on.
//!
//! The image is written next to the kernel (`<kernel>-uefi.img`). Firmware:
//! `$OVMF_FD` or the per-OS defaults (see `l_os_image::find_firmware`).
//! Extra arguments after the kernel are ignored (the kernel has no command
//! line), as with the former runner.

use l_os_image::{
    HostOs, Mode, Outcome, TEST_TIMEOUT, build_image, find_firmware, find_qemu, image_path_for, is_test_binary,
    project_root, qemu_args, run_exit, run_with_timeout, test_exit,
};
use std::{env, path::PathBuf, process::Command, process::ExitCode};

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("qemu_runner: {msg}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(kernel) = args.next() else {
        eprintln!("usage: l_os_run <kernel-elf> [args...]");
        return ExitCode::from(2);
    };
    let mode = if is_test_binary(&kernel.to_string_lossy()) { Mode::Test } else { Mode::Run };
    let cwd = match env::current_dir() {
        Ok(d) => d,
        Err(e) => return fail(format!("no current directory: {e}")),
    };
    let root = project_root(env::var_os("CARGO_MANIFEST_DIR"), cwd);

    let dirs: Vec<PathBuf> = env::var_os("PATH").map(|p| env::split_paths(&p).collect()).unwrap_or_default();
    let os = HostOs::current();
    let qemu = match find_qemu(os, &dirs, &|p| p.is_file()) {
        Ok(q) => q,
        Err(e) => return fail(e),
    };
    let firmware = match find_firmware(os, env::var_os("OVMF_FD").as_deref(), Some(&qemu), &|p| p.is_file()) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    // After QEMU and firmware are found: a missing QEMU fails fast.
    let image = image_path_for(&kernel);
    if let Err(e) = build_image(&PathBuf::from(&kernel), &image) {
        return fail(e);
    }
    let qargs = match qemu_args(mode, &image, &firmware, &root) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let mut cmd = Command::new(&qemu);
    cmd.args(&qargs);

    match mode {
        Mode::Test => {
            if let Err(e) = std::fs::create_dir_all(root.join("target")) {
                return fail(format!("cannot create {}: {e}", root.join("target").display()));
            }
            let outcome = match run_with_timeout(&mut cmd, TEST_TIMEOUT) {
                Ok(o) => o,
                Err(e) => return fail(format!("cannot start {}: {e}", qemu.display())),
            };
            let (code, msg) = test_exit(outcome, TEST_TIMEOUT);
            if let Some(m) = msg {
                eprintln!("{m}");
            }
            ExitCode::from(code)
        }
        Mode::Run => match cmd.status() {
            Ok(status) => ExitCode::from(run_exit(Outcome::from_status(status))),
            Err(e) => fail(format!("cannot start {}: {e}", qemu.display())),
        },
    }
}
