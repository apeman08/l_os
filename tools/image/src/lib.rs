//! Host-side tooling for l_os, shared by the two binaries of this crate:
//!
//! - `l_os_image <kernel-elf> <out.img>`: wrap a kernel ELF into a bootable
//!   UEFI disk image (plus `--print-firmware` / `--print-qemu` for scripts).
//! - `l_os_run <kernel-elf> [args...]`: the cargo runner for the kernel target
//!   (see `.cargo/config.toml`). It builds the image and boots it in QEMU.
//!
//! Everything that can be decided without QEMU (test-binary detection, the
//! QEMU command line, exit-code mapping, QEMU and firmware discovery) is a pure
//! function here so that it is unit-tested on the host:
//! `cargo test --manifest-path tools/image/Cargo.toml` from a neutral directory.
//!
//! Plain std only (no shell): the same code runs on Linux, macOS and Windows.
//! Paths marked [assumed] below were not tested on that OS.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

/// How long a kernel test may run before the runner kills QEMU.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// QEMU's exit code when the kernel wrote 0x10 to the isa-debug-exit port
/// (`(0x10 << 1) | 1`): the test passed.
pub const QEMU_SUCCESS: i32 = 33;

/// Build a bootable UEFI disk image from a kernel ELF (built against
/// `bootloader_api` 0.11).
///
/// No minimum framebuffer size is configured on purpose: with one, the UEFI
/// loader picks the *largest* GOP mode (2560x1600 under OVMF). Without it, the
/// current mode is kept, which QEMU sets via `-device VGA,xres=..,yres=..`.
pub fn build_image(kernel: &Path, out: &Path) -> Result<(), String> {
    let mut config = bootloader::BootConfig::default();
    // The kernel owns COM1: test output and the console mirror go there, and
    // the bootloader's log lines would mix into it. L_OS_BOOT_LOG=1 turns them
    // on for debugging boot problems (e.g. a bootloader panic before the kernel).
    config.serial_logging = std::env::var_os("L_OS_BOOT_LOG").is_some_and(|v| v == "1");
    bootloader::UefiBoot::new(kernel)
        .set_boot_config(&config)
        .create_disk_image(out)
        .map_err(|e| format!("cannot build {} from {}: {e:#}", out.display(), kernel.display()))
}

/// The image the runner writes next to a kernel ELF: `<kernel>-uefi.img`.
pub fn image_path_for(kernel: &OsStr) -> PathBuf {
    let mut s = kernel.to_os_string();
    s.push("-uefi.img");
    PathBuf::from(s)
}

/// Cargo names test executables `<name>-<16 lowercase hex digits>` (plus
/// `.exe` on Windows hosts; the kernel target has no extension, but be
/// tolerant). Both `/` and `\` count as directory separators.
pub fn is_test_binary(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    let name = match name.len().checked_sub(4) {
        Some(n) if name.is_char_boundary(n) && name[n..].eq_ignore_ascii_case(".exe") => &name[..n],
        _ => name,
    };
    let Some(n) = name.len().checked_sub(17) else { return false };
    let (stem, hash) = name.split_at(n);
    !stem.is_empty() && hash.starts_with('-') && hash[1..].bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// QEMU option values (`-drive file=...`) escape a comma by doubling it.
pub fn escape_qemu_value(value: &str) -> String {
    value.replace(',', ",,")
}

/// Kernel tests (headless, isa-debug-exit, packet capture) or `cargo run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Test,
    Run,
}

/// QEMU's arguments (without the program name). `root` is the repository root:
/// the test-mode packet capture goes to `<root>/target/net-trace.pcap`
/// (scripts/net_tests.sh inspects it).
pub fn qemu_args(mode: Mode, image: &Path, firmware: &Path, root: &Path) -> Result<Vec<OsString>, String> {
    let utf8 = |p: &Path| {
        p.to_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("path is not valid UTF-8 (needed for a QEMU option value): {}", p.display()))
    };
    // bootindex makes OVMF connect only the boot disk. Otherwise its own network
    // stack brings up the NIC during boot and sends IPv6 frames (MLD report,
    // duplicate-address probe) that the "nothing is sent" pcap check would see.
    // snapshot=on: the image is never written (it is rebuilt for every run anyway).
    let mut a: Vec<OsString> = vec![
        "-drive".into(),
        format!("if=none,id=disk0,format=raw,snapshot=on,file={}", escape_qemu_value(&utf8(image)?)).into(),
        "-device".into(),
        "ide-hd,drive=disk0,bootindex=0".into(),
        "-bios".into(),
        firmware.as_os_str().to_owned(),
    ];
    a.extend(
        [
            "-cpu",
            "max",
            "-vga",
            "none",
            "-device",
            "VGA,xres=1024,yres=768",
            "-netdev",
            "user,id=n0",
            "-device",
            "virtio-net-pci,netdev=n0,disable-modern=on",
            "-serial",
            "stdio",
        ]
        .map(OsString::from),
    );
    if mode == Mode::Test {
        let pcap = root.join("target").join("net-trace.pcap");
        a.push("-object".into());
        a.push(format!("filter-dump,id=f0,netdev=n0,file={}", escape_qemu_value(&utf8(&pcap)?)).into());
        a.extend(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-display", "none", "-no-reboot"].map(OsString::from));
    }
    Ok(a)
}

/// How QEMU ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Normal exit with this code.
    Exited(i32),
    /// Killed by a signal (Unix): there is no exit code.
    Signaled,
    /// The runner killed it after the timeout.
    TimedOut,
}

impl Outcome {
    pub fn from_status(status: ExitStatus) -> Self {
        match status.code() {
            Some(c) => Outcome::Exited(c),
            None => Outcome::Signaled,
        }
    }
}

/// Test mode: QEMU exit 33 -> runner exit 0; everything else -> 1 with a
/// message for stderr.
pub fn test_exit(outcome: Outcome, timeout: Duration) -> (u8, Option<String>) {
    match outcome {
        Outcome::Exited(QEMU_SUCCESS) => (0, None),
        Outcome::TimedOut => (1, Some(format!("qemu_runner: test timed out after {} s", timeout.as_secs()))),
        Outcome::Exited(c) => (1, Some(format!("qemu_runner: QEMU exited with code {c} (expected {QEMU_SUCCESS} = success)"))),
        Outcome::Signaled => (1, Some(format!("qemu_runner: QEMU was terminated by a signal (expected exit code {QEMU_SUCCESS} = success)"))),
    }
}

/// Run mode: QEMU's own exit status is the runner's (a code outside 0..=255 or
/// a signal becomes 1).
pub fn run_exit(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Exited(c) => u8::try_from(c).unwrap_or(1),
        Outcome::Signaled | Outcome::TimedOut => 1,
    }
}

/// Spawn `cmd` and wait at most `timeout` (std only: `try_wait` loop, kill on
/// timeout). stdio is whatever `cmd` was configured with.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> io::Result<Outcome> {
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome::from_status(status));
        }
        if Instant::now() >= deadline {
            // The child may exit between try_wait and kill; that is fine.
            // `Child::kill` is SIGKILL on Unix (TerminateProcess on Windows):
            // QEMU gets no chance to clean up, e.g. to restore a terminal it
            // put into raw mode (the former bash `timeout` sent SIGTERM). Only
            // test mode uses this, after the 300 s timeout.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Outcome::TimedOut);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The host OS, as far as QEMU and firmware discovery care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl HostOs {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            HostOs::Linux
        } else if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else if cfg!(windows) {
            HostOs::Windows
        } else {
            HostOs::Other
        }
    }
}

/// Where the official QEMU for Windows installer puts QEMU. [assumed: not
/// tested on Windows]
pub const WINDOWS_QEMU_DIR: &str = r"C:\Program Files\qemu";

/// Find `qemu-system-x86_64` (`.exe` on Windows): first in `path_dirs` (the
/// split `PATH`), then, on Windows, in [`WINDOWS_QEMU_DIR`].
pub fn find_qemu(os: HostOs, path_dirs: &[PathBuf], exists: &dyn Fn(&Path) -> bool) -> Result<PathBuf, String> {
    let name = if os == HostOs::Windows { "qemu-system-x86_64.exe" } else { "qemu-system-x86_64" };
    let defaults: &[&str] = if os == HostOs::Windows { &[WINDOWS_QEMU_DIR] } else { &[] };
    for dir in path_dirs.iter().map(PathBuf::as_path).chain(defaults.iter().map(Path::new)) {
        let candidate = dir.join(name);
        if exists(&candidate) {
            return Ok(candidate);
        }
    }
    let hint = match os {
        HostOs::Windows => format!(
            "install QEMU for Windows (https://qemu.weilnetz.de/) into {WINDOWS_QEMU_DIR} or put its directory on PATH"
        ),
        HostOs::MacOs => "brew install qemu".to_owned(),
        _ => "Debian/Ubuntu: apt-get install qemu-system-x86; Fedora: dnf install qemu-system-x86".to_owned(),
    };
    Err(format!(
        "{name} not found on PATH{}: {hint}",
        if os == HostOs::Windows { format!(" or in {WINDOWS_QEMU_DIR}") } else { String::new() }
    ))
}

/// Linux distribution paths of OVMF, in the order they are tried.
pub const LINUX_FIRMWARE: &[&str] = &[
    "/usr/share/qemu/OVMF.fd",
    "/usr/share/ovmf/OVMF.fd",
    "/usr/share/OVMF/OVMF.fd",
    "/usr/share/edk2/x64/OVMF.fd",
    "/usr/share/edk2-ovmf/x64/OVMF.fd",
];

/// Homebrew's QEMU ships edk2 firmware (Apple silicon prefix, then Intel).
/// [assumed: not tested on macOS]
pub const MACOS_FIRMWARE: &[&str] = &[
    "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
    "/usr/local/share/qemu/edk2-x86_64-code.fd",
];

/// The firmware for QEMU's `-bios`. `$OVMF_FD` wins (an error if that file does
/// not exist); otherwise the per-OS candidates are tried. On Windows the
/// candidate is `share\edk2-x86_64-code.fd` next to the QEMU executable.
pub fn find_firmware(
    os: HostOs,
    ovmf_fd: Option<&OsStr>,
    qemu: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    if let Some(v) = ovmf_fd.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        return if exists(&p) { Ok(p) } else { Err(format!("OVMF_FD={} does not exist", p.display())) };
    }
    let candidates: Vec<PathBuf> = match os {
        HostOs::Linux | HostOs::Other => LINUX_FIRMWARE.iter().map(PathBuf::from).collect(),
        HostOs::MacOs => MACOS_FIRMWARE.iter().map(PathBuf::from).collect(),
        // QEMU for Windows ships the edk2 firmware in `share` next to the
        // executable. [assumed: not tested on Windows]
        HostOs::Windows => qemu
            .and_then(Path::parent)
            .map(|d| d.join("share").join("edk2-x86_64-code.fd"))
            .into_iter()
            .collect(),
    };
    if let Some(found) = candidates.iter().find(|p| exists(p)) {
        return Ok(found.clone());
    }
    let hint = match os {
        HostOs::Windows => "the QEMU for Windows installer ships share\\edk2-x86_64-code.fd next to qemu-system-x86_64.exe",
        HostOs::MacOs => "brew install qemu (ships share/qemu/edk2-x86_64-code.fd)",
        _ => "Debian/Ubuntu: apt-get install ovmf; Fedora: dnf install edk2-ovmf; Arch: pacman -S edk2-ovmf",
    };
    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    Err(format!(
        "no OVMF (UEFI) firmware found. Set OVMF_FD=/path/to/OVMF.fd or install it ({hint}). Tried: {}",
        if tried.is_empty() { "nothing (QEMU not found)".to_owned() } else { tried.join(", ") }
    ))
}

/// The repository root for `target/` paths: `CARGO_MANIFEST_DIR` (cargo sets it
/// for processes it runs through `cargo run`/`cargo test`), else the current
/// directory (cargo runs test runners with the package root as cwd).
pub fn project_root(manifest_dir: Option<OsString>, cwd: PathBuf) -> PathBuf {
    match manifest_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn args_str(v: &[OsString]) -> Vec<String> {
        v.iter().map(|s| s.to_str().unwrap().to_owned()).collect()
    }

    // ---- (a) test-binary detection --------------------------------------

    #[test]
    fn test_binaries_are_detected_with_and_without_exe() {
        assert!(is_test_binary("target/x86_64-l_os/debug/deps/memory-0123456789abcdef"));
        assert!(is_test_binary("l_os-fedcba9876543210"));
        assert!(is_test_binary(r"target\x86_64-l_os\debug\deps\pci-0123456789abcdef.exe"));
        assert!(is_test_binary("deps/tls-0123456789abcdef.EXE"));
    }

    #[test]
    fn non_test_binaries_are_not_detected() {
        assert!(!is_test_binary("target/x86_64-l_os/debug/l_os"));
        assert!(!is_test_binary(r"target\x86_64-l_os\debug\l_os"));
        assert!(!is_test_binary(r"target\x86_64-l_os\debug\l_os.exe"));
        // 15 and 17 digits, uppercase hex, non-hex, missing dash
        assert!(!is_test_binary("memory-0123456789abcde"));
        assert!(!is_test_binary("memory-0123456789abcdef0"));
        assert!(!is_test_binary("memory-0123456789ABCDEF"));
        assert!(!is_test_binary("memory-0123456789abcdeg"));
        assert!(!is_test_binary("0123456789abcdef"));
        // the hash must be in the file name, not in a directory name
        assert!(!is_test_binary("x-0123456789abcdef/l_os"));
        assert!(!is_test_binary(""));
    }

    // ---- (b) QEMU arguments ---------------------------------------------

    #[test]
    fn commas_are_doubled() {
        assert_eq!(escape_qemu_value("a,b,,c"), "a,,b,,,,c");
        assert_eq!(escape_qemu_value("plain"), "plain");
    }

    const COMMON: &[&str] = &[
        "-drive",
        "if=none,id=disk0,format=raw,snapshot=on,file=/r/my,dir/k-uefi.img",
        "-device",
        "ide-hd,drive=disk0,bootindex=0",
        "-bios",
        "/fw/OVMF.fd",
        "-cpu",
        "max",
        "-vga",
        "none",
        "-device",
        "VGA,xres=1024,yres=768",
        "-netdev",
        "user,id=n0",
        "-device",
        "virtio-net-pci,netdev=n0,disable-modern=on",
        "-serial",
        "stdio",
    ];

    #[test]
    fn run_mode_args_match_the_old_runner() {
        let a = qemu_args(Mode::Run, Path::new("/r/my,dir/k-uefi.img"), Path::new("/fw/OVMF.fd"), Path::new("/r")).unwrap();
        let mut want: Vec<String> = COMMON.iter().map(|s| s.to_string()).collect();
        want[1] = "if=none,id=disk0,format=raw,snapshot=on,file=/r/my,,dir/k-uefi.img".into();
        assert_eq!(args_str(&a), want);
    }

    #[test]
    fn test_mode_adds_capture_debug_exit_and_headless() {
        let a = qemu_args(Mode::Test, Path::new("/r/my,dir/k-uefi.img"), Path::new("/fw/OVMF.fd"), Path::new("/r")).unwrap();
        let mut want: Vec<String> = COMMON.iter().map(|s| s.to_string()).collect();
        want[1] = "if=none,id=disk0,format=raw,snapshot=on,file=/r/my,,dir/k-uefi.img".into();
        let pcap = Path::new("/r").join("target").join("net-trace.pcap");
        want.extend([
            "-object".to_string(),
            format!("filter-dump,id=f0,netdev=n0,file={}", pcap.display()),
            "-device".into(),
            "isa-debug-exit,iobase=0xf4,iosize=0x04".into(),
            "-display".into(),
            "none".into(),
            "-no-reboot".into(),
        ]);
        assert_eq!(args_str(&a), want);
    }

    #[test]
    fn image_path_appends_suffix() {
        assert_eq!(image_path_for(OsStr::new("target/x/debug/l_os")), PathBuf::from("target/x/debug/l_os-uefi.img"));
    }

    // ---- (c) exit codes -------------------------------------------------

    #[test]
    fn test_mode_exit_mapping() {
        assert_eq!(test_exit(Outcome::Exited(33), TEST_TIMEOUT), (0, None));
        assert_eq!(
            test_exit(Outcome::TimedOut, TEST_TIMEOUT),
            (1, Some("qemu_runner: test timed out after 300 s".into()))
        );
        assert_eq!(
            test_exit(Outcome::Exited(35), TEST_TIMEOUT),
            (1, Some("qemu_runner: QEMU exited with code 35 (expected 33 = success)".into()))
        );
        assert_eq!(
            test_exit(Outcome::Exited(0), TEST_TIMEOUT),
            (1, Some("qemu_runner: QEMU exited with code 0 (expected 33 = success)".into()))
        );
        let (code, msg) = test_exit(Outcome::Signaled, TEST_TIMEOUT);
        assert_eq!(code, 1);
        assert!(msg.unwrap().contains("signal"));
    }

    #[test]
    fn run_mode_propagates_qemu_status() {
        assert_eq!(run_exit(Outcome::Exited(0)), 0);
        assert_eq!(run_exit(Outcome::Exited(3)), 3);
        assert_eq!(run_exit(Outcome::Exited(33)), 33);
        assert_eq!(run_exit(Outcome::Exited(-1)), 1);
        assert_eq!(run_exit(Outcome::Exited(256)), 1);
        assert_eq!(run_exit(Outcome::Signaled), 1);
        assert_eq!(run_exit(Outcome::TimedOut), 1);
    }

    // The test binary re-invokes itself as the child process: with
    // L_OS_IMAGE_TEST_CHILD set, this "test" sleeps or exits with a code.
    #[test]
    fn child_helper() {
        match std::env::var("L_OS_IMAGE_TEST_CHILD").as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(120)),
            Ok(code) => std::process::exit(code.parse().unwrap()),
            Err(_) => {}
        }
    }

    fn child(mode: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "tests::child_helper", "--nocapture", "--test-threads=1"])
            .env("L_OS_IMAGE_TEST_CHILD", mode)
            .stdout(std::process::Stdio::null());
        c
    }

    #[test]
    fn timeout_kills_the_child() {
        let start = Instant::now();
        let out = run_with_timeout(&mut child("sleep"), Duration::from_millis(500)).unwrap();
        assert_eq!(out, Outcome::TimedOut);
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(500), "returned too early: {took:?}");
        assert!(took < Duration::from_secs(30), "child was not killed in time: {took:?}");
    }

    #[test]
    fn exit_code_of_child_is_reported() {
        assert_eq!(run_with_timeout(&mut child("33"), Duration::from_secs(60)).unwrap(), Outcome::Exited(33));
        assert_eq!(run_with_timeout(&mut child("7"), Duration::from_secs(60)).unwrap(), Outcome::Exited(7));
    }

    // ---- (d) firmware discovery -----------------------------------------

    fn fs(files: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    #[test]
    fn ovmf_fd_env_wins_over_candidates() {
        let exists = fs(&["/my/OVMF.fd", "/usr/share/qemu/OVMF.fd"]);
        let fw = find_firmware(HostOs::Linux, Some(OsStr::new("/my/OVMF.fd")), None, &exists).unwrap();
        assert_eq!(fw, PathBuf::from("/my/OVMF.fd"));
    }

    #[test]
    fn missing_ovmf_fd_file_is_an_error_even_if_candidates_exist() {
        let exists = fs(&["/usr/share/qemu/OVMF.fd"]);
        let e = find_firmware(HostOs::Linux, Some(OsStr::new("/nope/OVMF.fd")), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_FD=/nope/OVMF.fd does not exist"), "{e}");
    }

    #[test]
    fn empty_ovmf_fd_is_ignored() {
        let exists = fs(&["/usr/share/ovmf/OVMF.fd"]);
        let fw = find_firmware(HostOs::Linux, Some(OsStr::new("")), None, &exists).unwrap();
        assert_eq!(fw, PathBuf::from("/usr/share/ovmf/OVMF.fd"));
    }

    #[test]
    fn linux_candidates_in_order() {
        let exists = fs(&["/usr/share/edk2-ovmf/x64/OVMF.fd", "/usr/share/OVMF/OVMF.fd"]);
        assert_eq!(find_firmware(HostOs::Linux, None, None, &exists).unwrap(), PathBuf::from("/usr/share/OVMF/OVMF.fd"));
        let exists = fs(&["/usr/share/qemu/OVMF.fd", "/usr/share/ovmf/OVMF.fd"]);
        assert_eq!(find_firmware(HostOs::Linux, None, None, &exists).unwrap(), PathBuf::from("/usr/share/qemu/OVMF.fd"));
    }

    #[test]
    fn macos_homebrew_candidates() {
        let exists = fs(&["/usr/local/share/qemu/edk2-x86_64-code.fd"]);
        assert_eq!(
            find_firmware(HostOs::MacOs, None, None, &exists).unwrap(),
            PathBuf::from("/usr/local/share/qemu/edk2-x86_64-code.fd")
        );
    }

    #[test]
    fn windows_firmware_next_to_qemu() {
        let qemu = Path::new(r"D:\tools\qemu").join("qemu-system-x86_64.exe");
        let fw = Path::new(r"D:\tools\qemu").join("share").join("edk2-x86_64-code.fd");
        let exists = |p: &Path| p == fw;
        assert_eq!(find_firmware(HostOs::Windows, None, Some(&qemu), &exists).unwrap(), fw);
    }

    #[test]
    fn missing_firmware_names_ovmf_fd_and_os_hint() {
        let none = |_: &Path| false;
        let e = find_firmware(HostOs::Linux, None, None, &none).unwrap_err();
        assert!(e.contains("OVMF_FD"), "{e}");
        assert!(e.contains("apt-get install ovmf"), "{e}");
        assert!(e.contains("/usr/share/edk2-ovmf/x64/OVMF.fd"), "lists what was tried: {e}");
        let qemu = Path::new(r"C:\Program Files\qemu").join("qemu-system-x86_64.exe");
        let e = find_firmware(HostOs::Windows, None, Some(&qemu), &none).unwrap_err();
        assert!(e.contains("OVMF_FD"), "{e}");
        assert!(e.contains("edk2-x86_64-code.fd"), "{e}");
        let e = find_firmware(HostOs::MacOs, None, None, &none).unwrap_err();
        assert!(e.contains("OVMF_FD") && e.contains("brew install qemu"), "{e}");
    }

    // ---- (e) QEMU lookup ------------------------------------------------

    #[test]
    fn qemu_found_on_path_in_order() {
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")];
        let exists = fs(&["/b/qemu-system-x86_64", "/c/qemu-system-x86_64"]);
        assert_eq!(find_qemu(HostOs::Linux, &dirs, &exists).unwrap(), PathBuf::from("/b/qemu-system-x86_64"));
    }

    #[test]
    fn qemu_on_windows_needs_exe_and_falls_back_to_program_files() {
        let dirs = vec![PathBuf::from(r"C:\Windows")];
        let in_path = PathBuf::from(r"C:\Windows").join("qemu-system-x86_64.exe");
        let e1 = in_path.clone();
        assert_eq!(find_qemu(HostOs::Windows, &dirs, &move |p: &Path| p == e1).unwrap(), in_path);
        let default = Path::new(WINDOWS_QEMU_DIR).join("qemu-system-x86_64.exe");
        let d1 = default.clone();
        assert_eq!(find_qemu(HostOs::Windows, &dirs, &move |p: &Path| p == d1).unwrap(), default);
    }

    #[test]
    fn qemu_default_dir_is_windows_only() {
        let default = Path::new(WINDOWS_QEMU_DIR).join("qemu-system-x86_64");
        let d1 = default.clone();
        assert!(find_qemu(HostOs::Linux, &[], &move |p: &Path| p == d1).is_err());
    }

    #[test]
    fn missing_qemu_is_a_clear_error() {
        let none = |_: &Path| false;
        let e = find_qemu(HostOs::Linux, &[PathBuf::from("/usr/bin")], &none).unwrap_err();
        assert!(e.contains("qemu-system-x86_64") && e.contains("PATH"), "{e}");
        let e = find_qemu(HostOs::Windows, &[], &none).unwrap_err();
        assert!(e.contains(WINDOWS_QEMU_DIR), "{e}");
    }

    // ---- root -----------------------------------------------------------

    #[test]
    fn root_prefers_manifest_dir() {
        assert_eq!(project_root(Some("/m".into()), "/cwd".into()), PathBuf::from("/m"));
        assert_eq!(project_root(None, "/cwd".into()), PathBuf::from("/cwd"));
        assert_eq!(project_root(Some("".into()), "/cwd".into()), PathBuf::from("/cwd"));
    }
}
