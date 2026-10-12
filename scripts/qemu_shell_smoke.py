#!/usr/bin/env python3
"""Boot the real kernel image in QEMU, type commands via the QEMU monitor and
assert on the VGA text buffer. This exercises the actual kernel, not host code.

Usage: cargo bootimage && python3 -I scripts/qemu_shell_smoke.py
Needs: qemu-system-x86_64. Characters are mapped to keys for the kernel's German layout.
"""
import os, socket, subprocess, sys, tempfile, time

IMG = "target/x86_64-l_os/debug/bootimage-l_os.bin"
# The kernel uses the German layout (De105Key): the US y/z keys are swapped.
KEYNAMES = {" ": "spc", "\n": "ret", "\b": "backspace", "y": "z", "z": "y"}

def main():
    tmp = tempfile.mkdtemp()
    sock_path, dump = os.path.join(tmp, "mon.sock"), os.path.join(tmp, "vga.bin")
    q = subprocess.Popen(["qemu-system-x86_64", "-drive", f"format=raw,file={IMG}",
                          "-display", "none", "-serial", "null", "-no-reboot",
                          "-monitor", f"unix:{sock_path},server,nowait"])
    try:
        for _ in range(100):
            if os.path.exists(sock_path): break
            time.sleep(0.1)
        m = socket.socket(socket.AF_UNIX); m.connect(sock_path); m.settimeout(2)
        def mon(c):
            m.sendall((c + "\n").encode()); time.sleep(0.15)
            try: m.recv(65536)
            except OSError: pass
        def type_(s):
            for ch in s:
                mon("sendkey " + KEYNAMES.get(ch, ch)); time.sleep(0.05)
        time.sleep(3)
        type_("help\n"); type_("echo hix\b\n"); type_("bogus\n")
        type_("help me\n"); type_("uptime\n"); type_("security\n")
        type_("run hello\n"); type_("run evil\n"); type_("run spin\n"); time.sleep(2); type_("run nope\n")
        time.sleep(1)
        mon(f'pmemsave 0xb8000 4000 "{dump}"'); time.sleep(1)
        d = open(dump, "rb").read()
    finally:
        q.terminate(); q.wait()
    rows = ["".join(chr(d[(r*80+c)*2]) if 32 <= d[(r*80+c)*2] < 127 else "." for c in range(80)).rstrip()
            for r in range(25)]
    screen = "\n".join(rows)
    print(screen)
    expected = ["l_os> echo hi", "\nhi\n", "unknown command: bogus",
                "help: unexpected arguments", "ticks (~", "no NIC driver", "ring 3",
                "hello from ring 3", "[process exited with code 18]", "illegal memory access",
                "exceeded its time budget", "unknown program 'nope'"]
    missing = [e for e in expected if e not in screen]
    if missing:
        print("FAIL, missing:", missing); sys.exit(1)
    print("OK")

main()
