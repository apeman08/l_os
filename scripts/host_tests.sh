#!/bin/sh
# Host-side tests for the portable crates (no QEMU involved).
#   l_core: parser, policy, handles, ELF parser, ...
#   l_tls : TLS client against a real `openssl s_server` (needs the openssl CLI)
# Runs from a neutral directory because cargo reads .cargo/config.toml (kernel
# target, build-std) from the *current* directory upward, not from the manifest.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
cd "$tmp"
cargo +stable test --manifest-path "$root/crates/l_core/Cargo.toml" --target-dir "$root/target/host-tests" "$@"
cargo +stable test --manifest-path "$root/crates/l_tls/Cargo.toml" --features std --target-dir "$root/target/host-tests" "$@"
