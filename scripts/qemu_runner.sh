#!/usr/bin/env bash
# Cargo runner for the kernel target (see .cargo/config.toml): replaces
# `bootimage runner`. Cargo calls it as `qemu_runner.sh <kernel-elf> [args...]`.
#
# - Test binaries (file name ends in `-<16 hex digits>`): boot headless, serial
#   on stdout, exit via isa-debug-exit. QEMU exit code 33 (the kernel wrote
#   0x10) means success -> 0; anything else, including the 300 s timeout -> 1.
#   QEMU's packet capture of the NIC goes to target/net-trace.pcap
#   (scripts/net_tests.sh inspects it).
# - Anything else (`cargo run`): boot the kernel interactively, serial on
#   stdout (until the framebuffer console exists, the console is mirrored there).
#
# Firmware: $OVMF_FD or the usual distribution paths (scripts/ovmf_path.sh).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)

if [ $# -lt 1 ]; then
  echo "usage: $0 <kernel-elf> [args...]" >&2
  exit 2
fi
kernel=$1
img="$kernel-uefi.img"
"$root/scripts/build_image.sh" "$kernel" "$img"
ovmf=$("$root/scripts/ovmf_path.sh")

common=(
  -drive "format=raw,file=$img"
  -bios "$ovmf"
  -cpu max
  -vga none -device VGA,xres=1024,yres=768
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on
  -serial stdio
)

if [[ "$(basename "$kernel")" =~ -[0-9a-f]{16}$ ]]; then
  mkdir -p "$root/target"
  set +e
  timeout 300 qemu-system-x86_64 "${common[@]}" \
    -object "filter-dump,id=f0,netdev=n0,file=$root/target/net-trace.pcap" \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none -no-reboot
  code=$?
  set -e
  if [ "$code" -eq 33 ]; then
    exit 0
  fi
  if [ "$code" -eq 124 ]; then
    echo "qemu_runner: test timed out after 300 s" >&2
  else
    echo "qemu_runner: QEMU exited with code $code (expected 33 = success)" >&2
  fi
  exit 1
fi

exec qemu-system-x86_64 "${common[@]}"
