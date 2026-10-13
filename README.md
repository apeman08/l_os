# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots via UEFI to a framebuffer terminal with an Aurora status and hint bar (milestone M1 done, see `docs/ROADMAP.md`) and runs sandboxed demo programs in ring 3 (separate address spaces, capability handles, IPC). Networking is denied by default (QEMU only): `net request HOST:PORT` asks you to confirm, then `run fetch https://HOST/PATH` can do an HTTPS GET (TLS 1.3 only). No storage. The shell is a ring-3 program. See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`, with `rust-src`), `qemu-system-x86_64`, OVMF UEFI firmware (Debian/Ubuntu: `apt-get install ovmf`; found at the usual paths, or set `OVMF_FD=/path/to/OVMF.fd` for a combined image, or `OVMF_CODE` + `OVMF_VARS` for a split code/variable-store pair). No shell is needed for `cargo run` / `cargo test`: the cargo runner is a Rust host program. The kernel build (`build.rs`) also compiles the ring-3 programs in `user/` and the host tools in `tools/image` (`l_os_image`, the UEFI image builder, and `l_os_run`, the cargo runner, into `target/l_os-tools/debug/`). The first build compiles the bootloader stages (`bootloader` 0.11) from crates.io: about a minute, needs network.

The kernel boots via UEFI and draws its console on the framebuffer (QEMU window, 1024x768). The serial mirror (cargo feature `serial-console`) is off: console text appears only on the framebuffer. `cargo run-serial` switches it on for development (every console write is also mirrored to the serial port, i.e. the terminal you start QEMU from), never for an amnesic image. Type into the QEMU window.

```
cargo run                                    # build, make a UEFI image, boot it in QEMU (runner: target/l_os-tools/debug/l_os_run)
cargo test                                   # kernel tests, each booted in QEMU (exit code 33 = pass)
scripts/host_tests.sh                        # host tests: l_core, l_tls (needs the openssl CLI), tools/image (runner logic)
scripts/net_tests.sh                         # network + TLS tests in QEMU against host test servers
python3 -I scripts/qemu_shell_smoke.py       # builds the image, drives the real kernel's shell, checks serial text and screen pixels
```
Only build the image (what the runner does before booting): `cargo build`, then `target/l_os-tools/debug/l_os_image target/x86_64-l_os/debug/l_os target/x86_64-l_os/debug/l_os-uefi.img`. `l_os_image --print-qemu`, `--print-firmware` (one file for a combined image, or code and variable store on two lines) and `--print-qemu-firmware-args` (QEMU's firmware arguments, one per line) print what the runner would use.

Firmware: a combined image (code and variables in one file, e.g. Debian's `/usr/share/ovmf/OVMF.fd`) is passed as `-bios <file>`. A split pair (e.g. `OVMF_CODE_4M.fd` + `OVMF_VARS_4M.fd`, or QEMU's own bundled `edk2-x86_64-code.fd` + `edk2-i386-vars.fd`) cannot be loaded with `-bios` (QEMU: "could not load PC BIOS"). It is attached as two pflash drives instead: the code read-only, the variable store with `snapshot=on`, so the template file is never written. Search order: `OVMF_FD`, then `OVMF_CODE` + `OVMF_VARS` (both must be set; if `OVMF_FD` is also set, `OVMF_FD` wins and the pair is ignored), then the combined Linux images, then split pairs. A pair counts only if both files exist.

To boot the image by hand (the same QEMU options `l_os_run` uses for `cargo run`; add `-display none` for a headless run with only the serial console), in bash:
```
mapfile -t fw < <(target/l_os-tools/debug/l_os_image --print-qemu-firmware-args)
qemu-system-x86_64 -drive if=none,id=disk0,format=raw,snapshot=on,file=target/x86_64-l_os/debug/l_os-uefi.img \
  -device ide-hd,drive=disk0,bootindex=0 "${fw[@]}" -cpu max -vga none -device VGA,xres=1024,yres=768 \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on -serial stdio
```

### Native Windows (untested)
`cargo run` and `cargo test` are meant to work natively on Windows (no WSL, no bash): runner, image build and firmware discovery are Rust (`tools/image`). **This has not been tested on Windows**; only the Linux side was run, and the tools crate was compile-checked for a Windows target (see below). What the runner assumes there:
- QEMU: install QEMU for Windows (e.g. the installer from https://qemu.weilnetz.de/). The runner looks for `qemu-system-x86_64.exe` on `PATH`, then in `C:\Program Files\qemu`. [assumed]
- Firmware: QEMU for Windows bundles a split edk2 pair, `share\edk2-x86_64-code.fd` + `share\edk2-i386-vars.fd`, next to `qemu-system-x86_64.exe`. If that is a Scoop/Chocolatey shim with no `share` next to it, the runner falls back to `C:\Program Files\qemu\share`. The runner attaches the pair as two pflash drives. It is not passed to `-bios`: an earlier version did that and failed on Windows with "could not load PC BIOS"; the same failure reproduces on Linux with a code-only image. The vars file name comes from QEMU's firmware descriptor `60-edk2-x86_64.json` and is [assumed] for Windows, since it was not tested there. Overrides: `set OVMF_FD=C:\path\to\OVMF.fd` (combined image, `-bios`), or `set OVMF_CODE=...` and `set OVMF_VARS=...` (split pair, pflash). If only the code file is found, the error names the missing vars file and `OVMF_VARS`.
- macOS: Homebrew's QEMU pair in `/opt/homebrew/share/qemu` or `/usr/local/share/qemu`, as pflash. [assumed]
- Cargo starts the runner as `target/l_os-tools/debug/l_os_run` relative to the repository; Rust's `std::process::Command` appends `.exe` to a path without an extension when that file exists (checked in the standard library source, not on Windows). [assumed]
- First build: as on Linux, building `tools/image` runs bootloader 0.11's build script, which itself runs a nested `cargo install` of the UEFI stage with build-std; it needs network and a host linker for `l_os_run.exe` (MSVC toolchain: Visual Studio Build Tools with the C++ workload). The nested target directories are deep: with a long checkout path this may exceed MAX_PATH (260 characters); enable long paths (`git config --system core.longpaths true` and the Windows `LongPathsEnabled` setting) or use a shorter checkout path. None of this was run on Windows. [assumed]
- Close any running QEMU / `cargo run` session before rebuilding after changes in `tools/image`: while `l_os_run.exe` runs, Windows locks the file and the rebuild fails ("Access is denied"). [assumed]
- If cargo says it cannot find or run `l_os_run` (the same "could not execute process" error as with the old bash runner), the tools were not built: run `cargo clean` (or touch `build.rs`) and build again. Deleting `target/l_os-tools` alone is handled (the next build rebuilds it; checked on Linux).
- The Python smoke tests and `scripts/*.sh` stay Unix-only (Unix-socket QEMU monitor, POSIX shell).
- `.gitattributes` keeps `*.sh` and `*.py` at LF line endings so that a Windows checkout does not break them for WSL/Git Bash use.

Checked on Linux: host unit tests of the runner logic (Windows paths included, as data), `cargo test`, `scripts/net_tests.sh`, both Python smoke tests, `cargo run`'s runner in run mode. Checked for Windows: `cargo check --target x86_64-pc-windows-gnu` of `tools/image` (compiles; not run).

Try `net status`, `run hello`, `run hello_rs`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).

Screenshot of the shell with the Aurora status and hint bars (a QEMU screendump of the real kernel screen, saved by `python3 -I scripts/qemu_shell_smoke.py --screenshot docs/screenshots/aurora-shell.png`): [docs/screenshots/aurora-shell.png](docs/screenshots/aurora-shell.png). It is a real screendump, not a mock-up, and its content (e.g. uptime) changes whenever it is regenerated.

Not done yet: real hardware, Tor, a browser and the amnesic live system (roadmap M2-M6). The status bar says "amnesisch: nein" on purpose.
