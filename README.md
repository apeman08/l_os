# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots to a minimal terminal and runs sandboxed demo programs in ring 3 (separate address spaces, capability handles, IPC). Networking is denied by default (QEMU only): `net request HOST:PORT` asks you to confirm, then `run fetch https://HOST/PATH` can do an HTTPS GET (TLS 1.3 only). No storage. The shell is a ring-3 program. See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`, with `rust-src`), `cargo install bootimage`, `qemu-system-x86_64`. The kernel build also compiles the ring-3 programs in `user/` (needs network on first build).

```
cargo bootimage
qemu-system-x86_64 -cpu max -drive format=raw,file=target/x86_64-l_os/debug/bootimage-l_os.bin   # -cpu max: SMEP/SMAP
cargo test                                   # kernel tests, booted in QEMU
scripts/host_tests.sh                        # host tests: l_core, l_tls (needs the openssl CLI)
scripts/net_tests.sh                         # network + TLS tests in QEMU against host test servers
python3 -I scripts/qemu_shell_smoke.py       # drives the real kernel's shell
```
Try `net status`, `run hello`, `run hello_rs`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).
