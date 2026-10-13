# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots via UEFI to a framebuffer terminal with an Aurora status and hint bar (milestone M1 done, see `docs/ROADMAP.md`) and runs sandboxed demo programs in ring 3 (separate address spaces, capability handles, IPC). Networking is denied by default (QEMU only): `net request HOST:PORT` asks you to confirm, then `run fetch https://HOST/PATH` can do an HTTPS GET (TLS 1.3 only). No storage. The shell is a ring-3 program. See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`, with `rust-src`), `qemu-system-x86_64`, OVMF UEFI firmware (Debian/Ubuntu: `apt-get install ovmf`; found at the usual paths, or set `OVMF_FD=/path/to/OVMF.fd`). No shell is needed for `cargo run` / `cargo test`: the cargo runner is a Rust host program. The kernel build (`build.rs`) also compiles the ring-3 programs in `user/` and the host tools in `tools/image` (`l_os_image`, the UEFI image builder, and `l_os_run`, the cargo runner, into `target/l_os-tools/debug/`). The first build compiles the bootloader stages (`bootloader` 0.11) from crates.io: about a minute, needs network.

The kernel boots via UEFI and draws its console on the framebuffer (QEMU window, 1024x768). By default (cargo feature `serial-console`) every console write is also mirrored to the serial port, i.e. the terminal you start QEMU from; build with `--no-default-features` to switch the mirror off. Type into the QEMU window.

```
cargo run                                    # build, make a UEFI image, boot it in QEMU (runner: target/l_os-tools/debug/l_os_run)
cargo test                                   # kernel tests, each booted in QEMU (exit code 33 = pass)
scripts/host_tests.sh                        # host tests: l_core, l_tls (needs the openssl CLI), tools/image (runner logic)
scripts/net_tests.sh                         # network + TLS tests in QEMU against host test servers
python3 -I scripts/qemu_shell_smoke.py       # builds the image, drives the real kernel's shell, checks serial text and screen pixels
```
Only build the image (what the runner does before booting): `cargo build`, then `target/l_os-tools/debug/l_os_image target/x86_64-l_os/debug/l_os target/x86_64-l_os/debug/l_os-uefi.img`. `l_os_image --print-qemu` and `l_os_image --print-firmware` print what the runner would use. To boot the image by hand (the same QEMU options `l_os_run` uses for `cargo run`; add `-display none` for a headless run with only the serial console):
```
qemu-system-x86_64 -bios "$(target/l_os-tools/debug/l_os_image --print-firmware)" -cpu max \
  -drive if=none,id=disk0,format=raw,snapshot=on,file=target/x86_64-l_os/debug/l_os-uefi.img \
  -device ide-hd,drive=disk0,bootindex=0 -vga none -device VGA,xres=1024,yres=768 \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on -serial stdio
```

### Native Windows (untested)
`cargo run` and `cargo test` are meant to work natively on Windows (no WSL, no bash): runner, image build and firmware discovery are Rust (`tools/image`). **This has not been tested on Windows**; only the Linux side was run, and the tools crate was compile-checked for a Windows target (see below). What the runner assumes there:
- QEMU: install QEMU for Windows (e.g. the installer from https://qemu.weilnetz.de/). The runner looks for `qemu-system-x86_64.exe` on `PATH`, then in `C:\Program Files\qemu`. [assumed]
- Firmware: `OVMF_FD` (e.g. `set OVMF_FD=C:\path\to\OVMF.fd`) wins; otherwise `share\edk2-x86_64-code.fd` next to the `qemu-system-x86_64.exe` found, which the QEMU for Windows installer ships, is passed to `-bios`. [assumed]
- Cargo starts the runner as `target/l_os-tools/debug/l_os_run` relative to the repository; Rust's `std::process::Command` appends `.exe` to a path without an extension when that file exists (checked in the standard library source, not on Windows). [assumed]
- The Python smoke tests and `scripts/*.sh` stay Unix-only (Unix-socket QEMU monitor, POSIX shell).
- `.gitattributes` keeps `*.sh` and `*.py` at LF line endings so that a Windows checkout does not break them for WSL/Git Bash use.

Checked on Linux: host unit tests of the runner logic (Windows paths included, as data), `cargo test`, `scripts/net_tests.sh`, both Python smoke tests, `cargo run`'s runner in run mode. Checked for Windows: `cargo check --target x86_64-pc-windows-gnu` of `tools/image` (compiles; not run).

Try `net status`, `run hello`, `run hello_rs`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).

Screenshot of the shell with the Aurora status and hint bars (a QEMU screendump of the real kernel screen, saved by `python3 -I scripts/qemu_shell_smoke.py --screenshot docs/screenshots/aurora-shell.png`): [docs/screenshots/aurora-shell.png](docs/screenshots/aurora-shell.png). It is a real screendump, not a mock-up, and its content (e.g. uptime) changes whenever it is regenerated.

Not done yet: real hardware, Tor, a browser and the amnesic live system (roadmap M2-M6). The status bar says "amnesisch: nein" on purpose.
