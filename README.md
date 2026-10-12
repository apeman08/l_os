# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots to a minimal terminal and runs sandboxed demo programs in ring 3 (separate address spaces, capability handles, IPC). Networking is denied by default (QEMU only): `net request HOST:PORT` asks you to confirm, then `run fetch https://HOST/PATH` can do an HTTPS GET (TLS 1.3 only). No storage. The shell is a ring-3 program. See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`, with `rust-src`), `qemu-system-x86_64`, OVMF UEFI firmware (Debian/Ubuntu: `apt-get install ovmf`; found at the usual paths, or set `OVMF_FD=/path/to/OVMF.fd`). The kernel build also compiles the ring-3 programs in `user/`, and the first image build compiles the bootloader (`bootloader` 0.11, host crate `tools/image`); both need network the first time.

The kernel boots via UEFI. Until the framebuffer console exists, the console is mirrored to the serial port, so the terminal you start QEMU from shows it; type into the QEMU window.

```
cargo run                                    # build, make a UEFI image, boot it in QEMU (scripts/qemu_runner.sh)
scripts/build_image.sh                       # only build: target/x86_64-l_os/debug/l_os-uefi.img
cargo test                                   # kernel tests, each booted in QEMU (exit code 33 = pass)
scripts/host_tests.sh                        # host tests: l_core, l_tls (needs the openssl CLI)
scripts/net_tests.sh                         # network + TLS tests in QEMU against host test servers
python3 -I scripts/qemu_shell_smoke.py       # drives the real kernel's shell (after scripts/build_image.sh)
```
Try `net status`, `run hello`, `run hello_rs`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).
