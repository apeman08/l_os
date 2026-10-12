#!/bin/sh
# Print the path of the OVMF (UEFI) firmware for QEMU's -bios.
# $OVMF_FD wins; otherwise the usual distribution paths are tried.
set -eu
if [ -n "${OVMF_FD:-}" ]; then
  if [ -f "$OVMF_FD" ]; then echo "$OVMF_FD"; exit 0; fi
  echo "OVMF_FD=$OVMF_FD does not exist" >&2
  exit 1
fi
for p in /usr/share/qemu/OVMF.fd /usr/share/ovmf/OVMF.fd /usr/share/OVMF/OVMF.fd /usr/share/edk2/x64/OVMF.fd /usr/share/edk2-ovmf/x64/OVMF.fd; do
  if [ -f "$p" ]; then echo "$p"; exit 0; fi
done
echo "no OVMF firmware found: install it (Debian/Ubuntu: apt-get install ovmf) or set OVMF_FD=/path/to/OVMF.fd" >&2
exit 1
