# Larp OS (`l_os`)

An experimental privacy-first operating system with its own Rust kernel (x86_64, QEMU target). Early stage: boots to a minimal terminal and can run demo programs in ring 3. **No network, no storage; process isolation is limited to one user process at a time.** See `docs/`.

## Build and run
Requires: rustup (nightly pinned by `rust-toolchain.toml`), `cargo install bootimage`, `qemu-system-x86_64`.

```
cargo bootimage
qemu-system-x86_64 -drive format=raw,file=target/x86_64-l_os/debug/bootimage-l_os.bin
cargo test                                   # kernel tests, booted in QEMU
scripts/host_tests.sh                        # host tests for crates/l_core
python3 -I scripts/qemu_shell_smoke.py       # drives the real kernel's shell
```
Try `run hello`, `run evil`, `run spin` in the shell. Keyboard layout is German (`De105Key`).
