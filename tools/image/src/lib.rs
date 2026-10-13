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

use l_core::bootmark::{self, BootId, MarkError};
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
    // The boot id is patched into a temporary copy of the kernel: the cargo
    // artifact itself stays untouched (so cargo's fingerprints and the next
    // run see the same file).
    let (tmp, id) = prepare_kernel_copy(kernel, out)?;
    let built = bootloader::UefiBoot::new(&tmp)
        .set_boot_config(&config)
        .create_disk_image(out)
        .map_err(|e| format!("cannot build {} from {}: {e:#}", out.display(), kernel.display()));
    let _ = std::fs::remove_file(&tmp);
    built?;
    if let Some(id) = id {
        let mut img = std::fs::read(out).map_err(|e| format!("cannot read {}: {e}", out.display()))?;
        write_lba0_marker(&mut img, &id)?;
        std::fs::write(out, &img).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    }
    Ok(())
}

/// Write the patched kernel next to `out` (`<out>.kernel.tmp`) and return its
/// path and the boot id (`None`: the kernel has no placeholder).
fn prepare_kernel_copy(kernel: &Path, out: &Path) -> Result<(PathBuf, Option<BootId>), String> {
    let bytes = std::fs::read(kernel).map_err(|e| format!("cannot read {}: {e}", kernel.display()))?;
    let (patched, id) = patch_kernel(&bytes).map_err(|e| format!("{}: {e}", kernel.display()))?;
    let mut tmp = out.as_os_str().to_os_string();
    tmp.push(".kernel.tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &patched).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    Ok((tmp, id))
}

/// Replace the boot-id placeholder in a kernel image with the embedded marker.
/// No placeholder: the kernel is returned unchanged with `None`; more than one:
/// error. The id is a hash of the kernel without the placeholder bytes, so it
/// is deterministic per kernel.
pub fn patch_kernel(kernel: &[u8]) -> Result<(Vec<u8>, Option<BootId>), String> {
    match bootmark::find_placeholder(kernel) {
        Err(MarkError::Missing) => Ok((kernel.to_vec(), None)),
        Err(MarkError::Duplicate) => Err("the boot-id placeholder occurs more than once in the kernel".into()),
        Ok(at) => {
            let id = bootmark::compute_id(kernel, at);
            let mut out = kernel.to_vec();
            out[at..at + 32].copy_from_slice(&bootmark::embedded(&id));
            Ok((out, Some(id)))
        }
    }
}

/// Write the stick marker to bytes 0..32 of LBA 0 (inside the protective MBR's
/// bootstrap area). Refuses unless those bytes are all zero: never overwrites.
pub fn write_lba0_marker(image: &mut [u8], id: &BootId) -> Result<(), String> {
    let Some(head) = image.get_mut(..32) else {
        return Err("disk image is smaller than 32 bytes".into());
    };
    if head.iter().any(|&b| b != 0) {
        return Err("LBA 0 bytes 0..32 are not zero; refusing to overwrite the bootstrap area".into());
    }
    head.copy_from_slice(&bootmark::lba0_marker(id));
    Ok(())
}

/// Tests named `usb_*` boot with an xHCI controller, the image attached as a
/// read-only USB stick (the one the kernel boots from) and a USB keyboard.
pub fn is_usb_test(path: &str) -> bool {
    path.rsplit(['/', '\\']).next().unwrap_or("").starts_with("usb_")
}

/// Extra QEMU arguments for `usb_*` tests (see `is_usb_test`): the same image
/// as a read-only stick on the xHCI, plus a keyboard.
pub fn usb_test_args(image: &Path) -> Vec<OsString> {
    let file = escape_qemu_value(&image.to_string_lossy());
    vec![
        "-device".into(),
        "qemu-xhci,id=xhci".into(),
        "-drive".into(),
        format!("if=none,id=stick,format=raw,readonly=on,file={file}").into(),
        "-device".into(),
        "usb-storage,bus=xhci.0,drive=stick,id=stick0".into(),
        "-device".into(),
        "usb-kbd,bus=xhci.0,id=kbd0".into(),
    ]
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
pub fn qemu_args(mode: Mode, image: &Path, firmware: &Firmware, root: &Path) -> Result<Vec<OsString>, String> {
    // bootindex makes OVMF connect only the boot disk. Otherwise its own network
    // stack brings up the NIC during boot and sends IPv6 frames (MLD report,
    // duplicate-address probe) that the "nothing is sent" pcap check would see.
    // snapshot=on: the image is never written (it is rebuilt for every run anyway).
    let mut a: Vec<OsString> = vec![
        "-drive".into(),
        format!("if=none,id=disk0,format=raw,snapshot=on,file={}", escape_qemu_value(&utf8(image)?)).into(),
        "-device".into(),
        "ide-hd,drive=disk0,bootindex=0".into(),
    ];
    a.extend(firmware_args(firmware)?);
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

/// Linux split images (code + variable store template), tried after the
/// combined images: Debian/Ubuntu's `ovmf` package (4 MiB build), then the
/// older 2 MiB names.
pub const LINUX_FIRMWARE_PAIRS: &[(&str, &str)] = &[
    ("/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
    ("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
];

/// Homebrew prefixes (Apple silicon, then Intel) whose `share/qemu` holds
/// QEMU's bundled edk2 pair. [assumed: not tested on macOS]
pub const MACOS_QEMU_SHARE: &[&str] = &["/opt/homebrew/share/qemu", "/usr/local/share/qemu"];

/// QEMU's bundled edk2 firmware is split: code image + variable store
/// template (QEMU's own descriptor `60-edk2-x86_64.json` pairs these names).
pub const EDK2_CODE: &str = "edk2-x86_64-code.fd";
pub const EDK2_VARS: &str = "edk2-i386-vars.fd";

/// UEFI firmware for QEMU. A combined image (code + variables in one file,
/// e.g. `OVMF.fd`) is loaded with `-bios`; a split pair cannot be (QEMU then
/// says "could not load PC BIOS") and goes onto two pflash drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Firmware {
    Bios(PathBuf),
    Pflash { code: PathBuf, vars: PathBuf },
}

/// The firmware overrides from the environment: `OVMF_FD` (combined image),
/// `OVMF_CODE` + `OVMF_VARS` (split pair). Empty values count as unset.
#[derive(Clone, Debug, Default)]
pub struct FirmwareEnv {
    pub ovmf_fd: Option<OsString>,
    pub ovmf_code: Option<OsString>,
    pub ovmf_vars: Option<OsString>,
}

impl FirmwareEnv {
    pub fn from_process() -> Self {
        FirmwareEnv {
            ovmf_fd: std::env::var_os("OVMF_FD"),
            ovmf_code: std::env::var_os("OVMF_CODE"),
            ovmf_vars: std::env::var_os("OVMF_VARS"),
        }
    }
}

/// QEMU's firmware arguments. `-bios <file>` for a combined image; for a pair
/// the code is a read-only pflash drive and the variable store a pflash drive
/// with snapshot=on, so the template file is never written.
pub fn firmware_args(firmware: &Firmware) -> Result<Vec<OsString>, String> {
    Ok(match firmware {
        Firmware::Bios(p) => vec!["-bios".into(), p.as_os_str().to_owned()],
        Firmware::Pflash { code, vars } => vec![
            "-drive".into(),
            format!("if=pflash,format=raw,unit=0,readonly=on,file={}", escape_qemu_value(&utf8(code)?)).into(),
            "-drive".into(),
            format!("if=pflash,format=raw,unit=1,snapshot=on,file={}", escape_qemu_value(&utf8(vars)?)).into(),
        ],
    })
}

/// A path as UTF-8, for QEMU option values that need comma escaping.
fn utf8(p: &Path) -> Result<String, String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8 (needed for a QEMU option value): {}", p.display()))
}

/// Where firmware may be, in the order tried after the environment overrides.
enum Candidate {
    Bios(PathBuf),
    Pair(PathBuf, PathBuf),
}

fn candidates(os: HostOs, qemu: Option<&Path>) -> Vec<Candidate> {
    let pair_in = |dir: &Path| Candidate::Pair(dir.join(EDK2_CODE), dir.join(EDK2_VARS));
    let mut c = Vec::new();
    match os {
        HostOs::Linux | HostOs::Other => {
            c.extend(LINUX_FIRMWARE.iter().map(|p| Candidate::Bios(p.into())));
            c.extend(LINUX_FIRMWARE_PAIRS.iter().map(|(code, vars)| Candidate::Pair(code.into(), vars.into())));
            // A QEMU in <prefix>/bin installs its bundled pair into
            // <prefix>/share/qemu. [assumed: no such QEMU on the test box]
            if let Some(prefix) = qemu.and_then(Path::parent).and_then(Path::parent) {
                c.push(pair_in(&prefix.join("share").join("qemu")));
            }
        }
        // [assumed: not tested on macOS]
        HostOs::MacOs => c.extend(MACOS_QEMU_SHARE.iter().map(|d| pair_in(Path::new(d)))),
        // QEMU for Windows ships the pair in `share` next to the executable;
        // then the default install dir, for a QEMU found through a shim
        // (Scoop, Chocolatey) whose directory has no `share`.
        // [assumed: the file names come from QEMU's descriptor; not tested on Windows]
        HostOs::Windows => {
            let install = Path::new(WINDOWS_QEMU_DIR);
            let next_to = qemu.and_then(Path::parent);
            c.extend(next_to.map(|d| pair_in(&d.join("share"))));
            if next_to != Some(install) {
                c.push(pair_in(&install.join("share")));
            }
        }
    }
    c
}

/// Find the UEFI firmware. Order: `$OVMF_FD` (combined; an error if the file
/// does not exist), then `$OVMF_CODE` + `$OVMF_VARS` (both required, each
/// must exist), then the combined Linux images, then split pairs (Linux
/// distribution pairs; QEMU's bundled pair in `share` next to the QEMU
/// executable on Windows [assumed], in the Homebrew prefixes on macOS
/// [assumed], in `<prefix>/share/qemu` for a QEMU in `<prefix>/bin` on Linux).
/// A pair counts only if both files exist.
pub fn find_firmware(
    os: HostOs,
    env: &FirmwareEnv,
    qemu: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<Firmware, String> {
    let set = |v: &Option<OsString>| v.as_ref().filter(|v| !v.is_empty()).map(PathBuf::from);
    let must_exist = |name: &str, p: PathBuf| {
        if exists(&p) { Ok(p) } else { Err(format!("{name}={} does not exist", p.display())) }
    };
    if let Some(p) = set(&env.ovmf_fd) {
        return must_exist("OVMF_FD", p).map(Firmware::Bios);
    }
    match (set(&env.ovmf_code), set(&env.ovmf_vars)) {
        (Some(code), Some(vars)) => {
            return Ok(Firmware::Pflash { code: must_exist("OVMF_CODE", code)?, vars: must_exist("OVMF_VARS", vars)? });
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(format!(
                "OVMF_CODE and OVMF_VARS must be set together (OVMF_CODE is {}, OVMF_VARS is {})",
                if set(&env.ovmf_code).is_some() { "set" } else { "not set" },
                if set(&env.ovmf_vars).is_some() { "set" } else { "not set" },
            ));
        }
        (None, None) => {}
    }

    let cands = candidates(os, qemu);
    for c in &cands {
        match c {
            Candidate::Bios(p) if exists(p) => return Ok(Firmware::Bios(p.clone())),
            Candidate::Pair(code, vars) if exists(code) && exists(vars) => {
                return Ok(Firmware::Pflash { code: code.clone(), vars: vars.clone() });
            }
            _ => {}
        }
    }
    if let Some(Candidate::Pair(code, vars)) = cands.iter().find(|c| matches!(c, Candidate::Pair(code, _) if exists(code))) {
        let example = if os == HostOs::Windows {
            r"set OVMF_CODE=C:\path\to\edk2-x86_64-code.fd and set OVMF_VARS=C:\path\to\edk2-i386-vars.fd (or set OVMF_FD=C:\path\to\OVMF.fd, a combined image)"
        } else {
            "set OVMF_CODE=/path/to/OVMF_CODE.fd and OVMF_VARS=/path/to/OVMF_VARS.fd (or OVMF_FD=/path/to/OVMF.fd, a combined image)"
        };
        return Err(format!(
            "found the UEFI code image {} but not its variable store {}; {example}",
            code.display(),
            vars.display()
        ));
    }
    let hint = match os {
        HostOs::Windows => {
            "the QEMU for Windows installer ships share\\edk2-x86_64-code.fd and share\\edk2-i386-vars.fd next to qemu-system-x86_64.exe"
        }
        HostOs::MacOs => "brew install qemu (ships share/qemu/edk2-x86_64-code.fd and edk2-i386-vars.fd)",
        _ => "Debian/Ubuntu: apt-get install ovmf; Fedora: dnf install edk2-ovmf; Arch: pacman -S edk2-ovmf",
    };
    let tried: Vec<String> = cands
        .iter()
        .map(|c| match c {
            Candidate::Bios(p) => p.display().to_string(),
            Candidate::Pair(code, vars) => format!("{} + {}", code.display(), vars.display()),
        })
        .collect();
    Err(format!(
        "no OVMF (UEFI) firmware found. Set OVMF_FD=/path/to/OVMF.fd (combined image) or OVMF_CODE + OVMF_VARS (split code and variable store), or install it ({hint}). Tried: {}",
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
        let a = qemu_args(Mode::Run, Path::new("/r/my,dir/k-uefi.img"), &Firmware::Bios("/fw/OVMF.fd".into()), Path::new("/r")).unwrap();
        let mut want: Vec<String> = COMMON.iter().map(|s| s.to_string()).collect();
        want[1] = "if=none,id=disk0,format=raw,snapshot=on,file=/r/my,,dir/k-uefi.img".into();
        assert_eq!(args_str(&a), want);
    }

    #[test]
    fn test_mode_adds_capture_debug_exit_and_headless() {
        let a = qemu_args(Mode::Test, Path::new("/r/my,dir/k-uefi.img"), &Firmware::Bios("/fw/OVMF.fd".into()), Path::new("/r")).unwrap();
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

    fn env(fd: Option<&str>, code: Option<&str>, vars: Option<&str>) -> FirmwareEnv {
        FirmwareEnv { ovmf_fd: fd.map(Into::into), ovmf_code: code.map(Into::into), ovmf_vars: vars.map(Into::into) }
    }

    fn bios(p: &str) -> Firmware {
        Firmware::Bios(PathBuf::from(p))
    }

    fn pflash(code: &str, vars: &str) -> Firmware {
        Firmware::Pflash { code: PathBuf::from(code), vars: PathBuf::from(vars) }
    }

    const PAIR4M: [&str; 2] = ["/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"];

    #[test]
    fn ovmf_fd_env_wins_over_everything() {
        let exists = fs(&["/my/OVMF.fd", "/c.fd", "/v.fd", "/usr/share/qemu/OVMF.fd", PAIR4M[0], PAIR4M[1]]);
        let e = env(Some("/my/OVMF.fd"), Some("/c.fd"), Some("/v.fd"));
        assert_eq!(find_firmware(HostOs::Linux, &e, None, &exists).unwrap(), bios("/my/OVMF.fd"));
    }

    #[test]
    fn missing_ovmf_fd_file_is_an_error_even_if_candidates_exist() {
        let exists = fs(&["/usr/share/qemu/OVMF.fd"]);
        let e = find_firmware(HostOs::Linux, &env(Some("/nope/OVMF.fd"), None, None), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_FD=/nope/OVMF.fd does not exist"), "{e}");
    }

    #[test]
    fn empty_ovmf_fd_is_ignored() {
        let exists = fs(&["/usr/share/ovmf/OVMF.fd"]);
        let fw = find_firmware(HostOs::Linux, &env(Some(""), Some(""), Some("")), None, &exists).unwrap();
        assert_eq!(fw, bios("/usr/share/ovmf/OVMF.fd"));
    }

    #[test]
    fn ovmf_code_and_vars_win_over_candidates() {
        let exists = fs(&["/c,1.fd", "/v.fd", "/usr/share/qemu/OVMF.fd"]);
        let fw = find_firmware(HostOs::Linux, &env(None, Some("/c,1.fd"), Some("/v.fd")), None, &exists).unwrap();
        assert_eq!(fw, pflash("/c,1.fd", "/v.fd"));
    }

    #[test]
    fn ovmf_code_without_vars_is_an_error() {
        let exists = fs(&["/c.fd", "/usr/share/qemu/OVMF.fd"]);
        let e = find_firmware(HostOs::Linux, &env(None, Some("/c.fd"), None), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_CODE") && e.contains("OVMF_VARS") && e.contains("together"), "{e}");
        let e = find_firmware(HostOs::Linux, &env(None, None, Some("/v.fd")), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_CODE") && e.contains("OVMF_VARS") && e.contains("together"), "{e}");
    }

    #[test]
    fn ovmf_code_or_vars_file_missing_is_an_error() {
        let exists = fs(&["/c.fd", "/usr/share/qemu/OVMF.fd"]);
        let e = find_firmware(HostOs::Linux, &env(None, Some("/c.fd"), Some("/nope.fd")), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_VARS=/nope.fd does not exist"), "{e}");
        let exists = fs(&["/v.fd"]);
        let e = find_firmware(HostOs::Linux, &env(None, Some("/nope.fd"), Some("/v.fd")), None, &exists).unwrap_err();
        assert!(e.contains("OVMF_CODE=/nope.fd does not exist"), "{e}");
    }

    #[test]
    fn linux_combined_candidates_in_order() {
        let exists = fs(&["/usr/share/edk2-ovmf/x64/OVMF.fd", "/usr/share/OVMF/OVMF.fd"]);
        assert_eq!(find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &exists).unwrap(), bios("/usr/share/OVMF/OVMF.fd"));
        let exists = fs(&["/usr/share/qemu/OVMF.fd", "/usr/share/ovmf/OVMF.fd"]);
        assert_eq!(find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &exists).unwrap(), bios("/usr/share/qemu/OVMF.fd"));
    }

    #[test]
    fn linux_combined_image_beats_pairs() {
        let exists = fs(&["/usr/share/edk2-ovmf/x64/OVMF.fd", PAIR4M[0], PAIR4M[1]]);
        assert_eq!(
            find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &exists).unwrap(),
            bios("/usr/share/edk2-ovmf/x64/OVMF.fd")
        );
    }

    #[test]
    fn linux_pairs_when_no_combined_image() {
        let exists = fs(&[PAIR4M[0], PAIR4M[1], "/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"]);
        assert_eq!(find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &exists).unwrap(), pflash(PAIR4M[0], PAIR4M[1]));
        let exists = fs(&["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"]);
        assert_eq!(
            find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &exists).unwrap(),
            pflash("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd")
        );
    }

    #[test]
    fn linux_qemu_prefix_pair() {
        let qemu = Path::new("/opt/q/bin/qemu-system-x86_64");
        let exists = fs(&["/opt/q/share/qemu/edk2-x86_64-code.fd", "/opt/q/share/qemu/edk2-i386-vars.fd"]);
        assert_eq!(
            find_firmware(HostOs::Linux, &FirmwareEnv::default(), Some(qemu), &exists).unwrap(),
            pflash("/opt/q/share/qemu/edk2-x86_64-code.fd", "/opt/q/share/qemu/edk2-i386-vars.fd")
        );
    }

    #[test]
    fn macos_homebrew_pair() {
        let exists = fs(&["/usr/local/share/qemu/edk2-x86_64-code.fd", "/usr/local/share/qemu/edk2-i386-vars.fd"]);
        assert_eq!(
            find_firmware(HostOs::MacOs, &FirmwareEnv::default(), None, &exists).unwrap(),
            pflash("/usr/local/share/qemu/edk2-x86_64-code.fd", "/usr/local/share/qemu/edk2-i386-vars.fd")
        );
    }

    #[test]
    fn windows_pair_next_to_qemu() {
        let dir = Path::new(r"D:\tools\qemu");
        let qemu = dir.join("qemu-system-x86_64.exe");
        let code = dir.join("share").join("edk2-x86_64-code.fd");
        let vars = dir.join("share").join("edk2-i386-vars.fd");
        let (c, v) = (code.clone(), vars.clone());
        let exists = move |p: &Path| p == c || p == v;
        assert_eq!(
            find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &exists).unwrap(),
            Firmware::Pflash { code, vars }
        );
    }

    #[test]
    fn code_without_vars_names_the_missing_vars_file() {
        let dir = Path::new(r"C:\Program Files\qemu");
        let qemu = dir.join("qemu-system-x86_64.exe");
        let code = dir.join("share").join("edk2-x86_64-code.fd");
        let vars = dir.join("share").join("edk2-i386-vars.fd");
        let c = code.clone();
        let e = find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &move |p: &Path| p == c).unwrap_err();
        assert!(e.contains(&vars.display().to_string()), "names the missing vars file: {e}");
        assert!(e.contains("OVMF_VARS"), "{e}");
        let e = find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &fs(&[PAIR4M[0]])).unwrap_err();
        assert!(e.contains(PAIR4M[1]) && e.contains("OVMF_VARS"), "{e}");
    }

    #[test]
    fn windows_pair_falls_back_to_install_dir_behind_a_shim() {
        let qemu = Path::new(r"C:\Users\u\scoop\shims").join("qemu-system-x86_64.exe");
        let share = Path::new(WINDOWS_QEMU_DIR).join("share");
        let (code, vars) = (share.join("edk2-x86_64-code.fd"), share.join("edk2-i386-vars.fd"));
        let (c, v) = (code.clone(), vars.clone());
        let exists = move |p: &Path| p == c || p == v;
        assert_eq!(
            find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &exists).unwrap(),
            Firmware::Pflash { code: code.clone(), vars: vars.clone() }
        );
        let e = find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &|_: &Path| false).unwrap_err();
        let shim_code = Path::new(r"C:\Users\u\scoop\shims").join("share").join("edk2-x86_64-code.fd");
        assert!(e.contains(&shim_code.display().to_string()), "lists the pair next to qemu: {e}");
        assert!(e.contains(&code.display().to_string()) && e.contains(&vars.display().to_string()), "lists the install dir pair: {e}");
    }

    #[test]
    fn windows_install_dir_pair_is_not_listed_twice() {
        let qemu = Path::new(WINDOWS_QEMU_DIR).join("qemu-system-x86_64.exe");
        let e = find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &|_: &Path| false).unwrap_err();
        assert_eq!(e.matches("edk2-x86_64-code.fd +").count(), 1, "{e}");
    }

    #[test]
    fn missing_vars_hint_is_per_os() {
        let qemu = Path::new(WINDOWS_QEMU_DIR).join("qemu-system-x86_64.exe");
        let code = Path::new(WINDOWS_QEMU_DIR).join("share").join("edk2-x86_64-code.fd");
        let e = find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &move |p: &Path| p == code).unwrap_err();
        assert!(e.contains(r"set OVMF_VARS=C:\path\to\edk2-i386-vars.fd"), "{e}");
        assert!(!e.contains("/path/to"), "no Unix placeholder on Windows: {e}");
        let e = find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &fs(&[PAIR4M[0]])).unwrap_err();
        assert!(e.contains("OVMF_VARS=/path/to/OVMF_VARS.fd"), "{e}");
    }

    #[test]
    fn missing_firmware_names_overrides_and_os_hint() {
        let none = |_: &Path| false;
        let e = find_firmware(HostOs::Linux, &FirmwareEnv::default(), None, &none).unwrap_err();
        assert!(e.contains("OVMF_FD") && e.contains("OVMF_CODE") && e.contains("OVMF_VARS"), "{e}");
        assert!(e.contains("apt-get install ovmf"), "{e}");
        assert!(e.contains("/usr/share/edk2-ovmf/x64/OVMF.fd"), "lists what was tried: {e}");
        assert!(e.contains(PAIR4M[0]) && e.contains(PAIR4M[1]), "lists pairs: {e}");
        let qemu = Path::new(r"C:\Program Files\qemu").join("qemu-system-x86_64.exe");
        let e = find_firmware(HostOs::Windows, &FirmwareEnv::default(), Some(&qemu), &none).unwrap_err();
        assert!(e.contains("OVMF_FD") && e.contains("edk2-x86_64-code.fd") && e.contains("edk2-i386-vars.fd"), "{e}");
        let e = find_firmware(HostOs::MacOs, &FirmwareEnv::default(), None, &none).unwrap_err();
        assert!(e.contains("OVMF_FD") && e.contains("brew install qemu"), "{e}");
    }

    #[test]
    fn bios_args() {
        assert_eq!(args_str(&firmware_args(&bios("/fw/my,OVMF.fd")).unwrap()), ["-bios", "/fw/my,OVMF.fd"]);
    }

    #[test]
    fn pflash_args_escape_commas() {
        let a = firmware_args(&pflash("/fw/c,ode.fd", "/fw/v,ars.fd")).unwrap();
        assert_eq!(
            args_str(&a),
            [
                "-drive",
                "if=pflash,format=raw,unit=0,readonly=on,file=/fw/c,,ode.fd",
                "-drive",
                "if=pflash,format=raw,unit=1,snapshot=on,file=/fw/v,,ars.fd",
            ]
        );
    }

    #[test]
    fn pflash_replaces_bios_in_full_command_line() {
        let a = qemu_args(Mode::Run, Path::new("/r/k-uefi.img"), &pflash("/c.fd", "/v.fd"), Path::new("/r")).unwrap();
        let a = args_str(&a);
        assert!(!a.iter().any(|x| x == "-bios"));
        assert_eq!(a[4..8], ["-drive", "if=pflash,format=raw,unit=0,readonly=on,file=/c.fd", "-drive", "if=pflash,format=raw,unit=1,snapshot=on,file=/v.fd"]);
        assert_eq!(a[8..10], ["-cpu", "max"]);
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


    // ---- boot id marker ---------------------------------------------------

    fn kernel_with_placeholder() -> Vec<u8> {
        let mut k = b"ELF-ish head".to_vec();
        k.extend_from_slice(&l_core::bootmark::PLACEHOLDER);
        k.extend_from_slice(b"tail bytes");
        k
    }

    #[test]
    fn patch_replaces_placeholder_with_embedded_id() {
        use l_core::bootmark::{compute_id, embedded, find_placeholder, parse_embedded};
        let k = kernel_with_placeholder();
        let at = find_placeholder(&k).unwrap();
        let (patched, id) = patch_kernel(&k).unwrap();
        let id = id.expect("id");
        assert_eq!(id, compute_id(&k, at));
        assert_eq!(patched.len(), k.len());
        assert_eq!(&patched[at..at + 32], &embedded(&id));
        assert_eq!(parse_embedded(patched[at..at + 32].try_into().unwrap()), Some(id));
        assert_eq!(&patched[..at], &k[..at]);
        assert_eq!(&patched[at + 32..], &k[at + 32..]);
        // deterministic, and the input slice is untouched
        assert_eq!(patch_kernel(&k).unwrap(), (patched, Some(id)));
        assert_eq!(k, kernel_with_placeholder());
    }

    #[test]
    fn patch_without_placeholder_is_noop() {
        let k = b"no marker in here at all".to_vec();
        assert_eq!(patch_kernel(&k).unwrap(), (k, None));
    }

    #[test]
    fn patch_with_two_placeholders_fails() {
        let mut k = kernel_with_placeholder();
        k.extend_from_slice(&l_core::bootmark::PLACEHOLDER);
        assert!(patch_kernel(&k).is_err());
    }

    #[test]
    fn lba0_marker_written_at_offset_0() {
        use l_core::bootmark::{lba0_marker, parse_lba0};
        let id = [9u8; 16];
        let mut img = vec![0u8; 1024];
        img[32] = 0x55; // protective MBR data after the bootstrap marker area
        img[510] = 0x55;
        img[511] = 0xAA;
        write_lba0_marker(&mut img, &id).unwrap();
        assert_eq!(&img[..32], &lba0_marker(&id));
        assert_eq!(parse_lba0(&img), Some(id));
        assert_eq!((img[32], img[510], img[511]), (0x55, 0x55, 0xAA));
        assert!(img[512..].iter().all(|&b| b == 0));
    }

    #[test]
    fn lba0_nonzero_bootstrap_is_refused() {
        for pos in [0usize, 17, 31] {
            let mut img = vec![0u8; 512];
            img[pos] = 1;
            let before = img.clone();
            assert!(write_lba0_marker(&mut img, &[1; 16]).is_err());
            assert_eq!(img, before, "never overwrite");
        }
        assert!(write_lba0_marker(&mut [0u8; 31], &[1; 16]).is_err());
    }

    #[test]
    fn build_image_patches_a_temp_copy_only() {
        // The helper that prepares the kernel copy must not touch the source.
        let dir = std::env::temp_dir().join(format!("l_os_image_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("kernel");
        let k = kernel_with_placeholder();
        std::fs::write(&src, &k).unwrap();
        let out = dir.join("k-uefi.img");
        let (tmp, id) = prepare_kernel_copy(&src, &out).unwrap();
        assert!(id.is_some());
        assert_eq!(std::fs::read(&src).unwrap(), k);
        assert_ne!(std::fs::read(&tmp).unwrap(), k);
        assert_ne!(tmp, src);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- USB tests ----------------------------------------------------------

    #[test]
    fn usb_test_detection() {
        assert!(is_usb_test("target/x86_64-l_os/debug/deps/usb_boot-0123456789abcdef"));
        assert!(is_usb_test(r"deps\usb_xhci-0123456789abcdef.exe"));
        assert!(is_usb_test("usb_x"));
        assert!(!is_usb_test("target/x86_64-l_os/debug/deps/memory-0123456789abcdef"));
        assert!(!is_usb_test("deps/my_usb_boot-0123456789abcdef"));
        assert!(!is_usb_test("usb/memory-0123456789abcdef"));
        assert!(!is_usb_test("usbx_boot-0123456789abcdef"));
        assert!(!is_usb_test(""));
    }

    #[test]
    fn usb_args_attach_same_image_readonly() {
        let a = args_str(&usb_test_args(Path::new("/r/my,dir/k-uefi.img")));
        assert_eq!(
            a,
            [
                "-device",
                "qemu-xhci,id=xhci",
                "-drive",
                "if=none,id=stick,format=raw,readonly=on,file=/r/my,,dir/k-uefi.img",
                "-device",
                "usb-storage,bus=xhci.0,drive=stick,id=stick0",
                "-device",
                "usb-kbd,bus=xhci.0,id=kbd0",
            ]
        );
    }

    #[test]
    fn test_mode_args_unchanged_for_other_tests() {
        let a = args_str(&qemu_args(Mode::Test, Path::new("/r/k-uefi.img"), &Firmware::Bios("/fw/OVMF.fd".into()), Path::new("/r")).unwrap());
        assert!(!a.iter().any(|x| x.contains("xhci") || x.contains("usb") || x.contains("stick")), "{a:?}");
        assert!(a.iter().any(|x| x == "isa-debug-exit,iobase=0xf4,iosize=0x04"));
    }

    // ---- root -----------------------------------------------------------

    #[test]
    fn root_prefers_manifest_dir() {
        assert_eq!(project_root(Some("/m".into()), "/cwd".into()), PathBuf::from("/m"));
        assert_eq!(project_root(None, "/cwd".into()), PathBuf::from("/cwd"));
        assert_eq!(project_root(Some("".into()), "/cwd".into()), PathBuf::from("/cwd"));
    }
}
