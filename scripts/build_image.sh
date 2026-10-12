#!/usr/bin/env bash
# Build a bootable UEFI disk image from a kernel ELF.
#   scripts/build_image.sh <kernel-elf> <output.img>
#   scripts/build_image.sh            (builds the kernel, writes $DEFAULT_IMG)
#
# The image builder (tools/image, a host crate depending on `bootloader` 0.11)
# is compiled from a neutral directory with a scrubbed environment so that the
# kernel's .cargo/config.toml (custom target, build-std) and the variables
# cargo passes to runners do not leak into the host build. Its first build
# compiles the bootloader stages from crates.io (about a minute, needs network).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
DEFAULT_IMG="$root/target/x86_64-l_os/debug/l_os-uefi.img"

if [ $# -eq 0 ]; then
  (cd "$root" && cargo build)
  set -- "$root/target/x86_64-l_os/debug/l_os" "$DEFAULT_IMG"
fi
if [ $# -ne 2 ]; then
  echo "usage: $0 [<kernel-elf> <output.img>]" >&2
  exit 2
fi
kernel=$1
out=$2

# The bootloader stages need the pinned nightly (build-std); take it from the
# repository's rust-toolchain.toml rather than from the current directory.
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")
if [ -z "$toolchain" ]; then
  echo "build_image.sh: no channel in $root/rust-toolchain.toml" >&2
  exit 1
fi

scrub=()
while IFS='=' read -r name _; do
  case "$name" in
    CARGO_HOME | CARGO_HTTP_* | CARGO_NET_* | CARGO_REGISTRIES_*) ;;
    CARGO | CARGO_* | RUSTC | RUSTC_* | RUSTDOC | RUSTDOC_* | RUSTFLAGS | RUSTUP_TOOLCHAIN | __CARGO_*) scrub+=(-u "$name") ;;
  esac
done < <(env)

builder_target="$root/target/image-builder"
(cd / && env "${scrub[@]}" cargo "+$toolchain" build --quiet --locked \
  --manifest-path "$root/tools/image/Cargo.toml" --target-dir "$builder_target")
"$builder_target/debug/l_os_image" "$kernel" "$out"
