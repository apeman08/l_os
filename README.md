# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots via UEFI to a framebuffer terminal with an Aurora status and hint bar (milestone M1 done, see `docs/ROADMAP.md`) and runs sandboxed demo programs in ring 3 (separate address spaces, capability handles, IPC). Networking is denied by default (QEMU only): `net request HOST:PORT` asks you to confirm, then `run fetch https://HOST/PATH` can do an HTTPS GET (TLS 1.3 only). No storage. The shell is a ring-3 program. See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`, with `rust-src`), `qemu-system-x86_64`, OVMF UEFI firmware (Debian/Ubuntu: `apt-get install ovmf`; found at the usual paths, or set `OVMF_FD=/path/to/OVMF.fd`). The kernel build also compiles the ring-3 programs in `user/`, and the first image build compiles the bootloader (`bootloader` 0.11, host crate `tools/image`); both need network the first time.

The kernel boots via UEFI and draws its console on the framebuffer (QEMU window, 1024x768). By default (cargo feature `serial-console`) every console write is also mirrored to the serial port, i.e. the terminal you start QEMU from; build with `--no-default-features` to switch the mirror off. Type into the QEMU window.

```
cargo run                                    # build, make a UEFI image, boot it in QEMU (scripts/qemu_runner.sh)
scripts/build_image.sh                       # only build: target/x86_64-l_os/debug/l_os-uefi.img
cargo test                                   # kernel tests, each booted in QEMU (exit code 33 = pass)
scripts/host_tests.sh                        # host tests: l_core, l_tls (needs the openssl CLI)
scripts/net_tests.sh                         # network + TLS tests in QEMU against host test servers
python3 -I scripts/qemu_shell_smoke.py       # drives the real kernel's shell, checks serial text and screen pixels (after scripts/build_image.sh)
```
To boot the image from `scripts/build_image.sh` by hand (the same QEMU options `scripts/qemu_runner.sh` uses for `cargo run`; `scripts/ovmf_path.sh` honours `OVMF_FD`; add `-display none` for a headless run with only the serial console):
```
qemu-system-x86_64 -bios "$(scripts/ovmf_path.sh)" -cpu max \
  -drive if=none,id=disk0,format=raw,snapshot=on,file=target/x86_64-l_os/debug/l_os-uefi.img \
  -device ide-hd,drive=disk0,bootindex=0 -vga none -device VGA,xres=1024,yres=768 \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on -serial stdio
```
Try `net status`, `run hello`, `run hello_rs`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).

Screenshot of the shell with the Aurora status and hint bars (a QEMU screendump of the real kernel screen, saved by `python3 -I scripts/qemu_shell_smoke.py --screenshot docs/screenshots/aurora-shell.png`): [docs/screenshots/aurora-shell.png](docs/screenshots/aurora-shell.png). It is a real screendump, not a mock-up, and its content (e.g. uptime) changes whenever it is regenerated.

Not done yet: real hardware, Tor, a browser and the amnesic live system (roadmap M2-M6). The status bar says "amnesisch: nein" on purpose.
