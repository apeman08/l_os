#!/usr/bin/env python3
"""Drive `net request` and `run fetch` in the real OS under QEMU.

Needs the host test servers from scripts/net_tests.sh (DNS responder on
127.0.0.1:5353 and an openssl TLS server on 127.0.0.1:18443; start them first,
e.g. `scripts/net_tests.sh` leaves nothing running, so start them by hand).

`fetch` uses the PRODUCTION root store (webpki-roots), so the local test
server's certificate (issued by a test CA) MUST be refused. What is asserted:
fail-closed behaviour on the real shell. The success path with a trusted CA is
covered by tests/tls.rs (same code, test root store).

Image: scripts/build_image.sh (UEFI, needs OVMF; see scripts/ovmf_path.sh).
The console is read from the kernel's serial mirror (no VGA text buffer under UEFI).
"""
import os, re, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IMG = os.path.join(ROOT, "target/x86_64-l_os/debug/l_os-uefi.img")
KEYNAMES = {" ": "spc", "\n": "ret", "\b": "backspace", "y": "z", "z": "y", "_": "shift-slash",
            ":": "shift-dot", ".": "dot", "/": "shift-7"}
ANSI = re.compile(r"\x1b(\[[0-9;=?]*[A-Za-z]|[()][0-9A-Za-z])")

def ovmf():
    return subprocess.run([os.path.join(ROOT, "scripts/ovmf_path.sh")], check=True,
                          capture_output=True, text=True).stdout.strip()

def render(raw):
    """Serial bytes -> screen lines (backspace erases, escape sequences dropped)."""
    lines = [""]
    for ch in ANSI.sub("", raw.decode("latin-1")).replace("\r", ""):
        if ch == "\n": lines.append("")
        elif ch == "\b": lines[-1] = lines[-1][:-1]
        else: lines[-1] += ch if 32 <= ord(ch) < 127 else "."
    return lines

def main():
    tmp = tempfile.mkdtemp()
    sock_path, serial = os.path.join(tmp, "mon.sock"), os.path.join(tmp, "serial.log")
    q = subprocess.Popen(["qemu-system-x86_64", "-drive", f"if=none,id=disk0,format=raw,file={IMG},snapshot=on",
                          # bootindex: OVMF then connects only the disk and leaves the NIC alone.
                          "-device", "ide-hd,drive=disk0,bootindex=0", "-bios", ovmf(),
                          "-vga", "none", "-device", "VGA,xres=1024,yres=768", "-cpu", "max",
                          "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
                          "-display", "none", "-serial", f"file:{serial}", "-no-reboot",
                          "-monitor", f"unix:{sock_path},server,nowait"])
    snaps = []
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
                mon("sendkey " + KEYNAMES.get(ch, ch)); time.sleep(0.04)
        def snap():
            """Take the console output so far (the serial mirror holds all of it)."""
            time.sleep(1)
            snaps[:] = [r.rstrip() for r in render(open(serial, "rb").read())]
        def wait_for(text, seconds):
            end = time.time() + seconds
            n = max(len(snaps) - 1, 0)  # the last line may still have been growing
            while time.time() < end:
                snap()
                if any(text in r for r in snaps[n:]): return True
                time.sleep(1)
            return False

        # UEFI firmware + bootloader take a few seconds; wait for the prompt.
        if not wait_for("l_os>", 60):
            print("FAIL: no shell prompt on the serial console"); sys.exit(1)
        type_("net resolver 10.0.2.2:5353\n"); time.sleep(0.6); type_("y")
        type_("net request 10.0.2.2:5353 120\n"); time.sleep(0.6); type_("y")
        type_("net request test.example:18443 120\n"); time.sleep(0.6); type_("y"); time.sleep(0.5)
        snap()
        type_("clear\n"); type_("run fetch http://test.example/\n"); time.sleep(1)
        type_("run fetch https://test.example:18444/\n"); time.sleep(1.5); snap()
        type_("clear\n"); type_("run fetch https://test.example:18443/\n")
        wait_for("[process exited", 90); snap()
        type_("clear\n"); type_("net request 1.1.1.1:443 120\n"); time.sleep(0.6); type_("y"); time.sleep(0.5)
        type_("run fetch https://1.1.1.1/\n"); wait_for("[process exited", 90); snap()
    finally:
        q.terminate(); q.wait()
    screen = "\n".join(snaps)  # serial lines are whole lines; no row-wrap to undo
    print("\n".join(r for r in snaps if r.strip()))
    expected = [
        "only https:// URLs are supported",                       # no plain http
        "not authorized: run 'net request test.example:18444'",   # no grant -> nothing happens
        "authority this system does not trust",                   # production roots refuse the test CA
    ]
    missing = [e for e in expected if e not in screen]
    if missing:
        print("FAIL, missing:", missing); sys.exit(1)
    print("OK (fail-closed behaviour verified on the real shell)")

main()
