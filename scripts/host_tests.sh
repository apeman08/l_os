#!/bin/sh
# Host-side tests for the portable crates (no QEMU involved).
#   l_core: parser, policy, handles, ELF parser, ...
#   l_tls : TLS client against a real `openssl s_server` (needs the openssl CLI)
#   tools/image: cargo runner / image builder logic (QEMU command line, exit
#           codes, QEMU and firmware discovery). Needs the pinned nightly from
#           rust-toolchain.toml (the bootloader dependency builds with build-std);
#           shares target/l_os-tools with the build that build.rs runs.
# Runs from a neutral directory because cargo reads .cargo/config.toml (kernel
# target, build-std) from the *current* directory upward, not from the manifest.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
cd "$tmp"
cargo +stable test --manifest-path "$root/crates/l_core/Cargo.toml" --target-dir "$root/target/host-tests" "$@"
cargo +stable test --manifest-path "$root/crates/l_tls/Cargo.toml" --features std --target-dir "$root/target/host-tests" "$@"
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")
if [ -z "$toolchain" ]; then
  echo "host_tests.sh: no channel in $root/rust-toolchain.toml" >&2
  exit 1
fi
cargo "+$toolchain" test --locked --manifest-path "$root/tools/image/Cargo.toml" --target-dir "$root/target/l_os-tools" "$@"
