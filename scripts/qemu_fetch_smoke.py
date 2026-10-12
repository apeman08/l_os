#!/usr/bin/env python3
"""Drive `net request` and `run fetch` in the real OS under QEMU.

Needs the host test servers from scripts/net_tests.sh (DNS responder on
127.0.0.1:5353 and an openssl TLS server on 127.0.0.1:18443; start them first,
e.g. `scripts/net_tests.sh` leaves nothing running, so start them by hand).

`fetch` uses the PRODUCTION root store (webpki-roots), so the local test
server's certificate (issued by a test CA) MUST be refused. What is asserted:
fail-closed behaviour on the real shell. The success path with a trusted CA is
covered by tests/tls.rs (same code, test root store).
"""
import os, socket, subprocess, sys, tempfile, time

IMG = "target/x86_64-l_os/debug/bootimage-l_os.bin"
KEYNAMES = {" ": "spc", "\n": "ret", "\b": "backspace", "y": "z", "z": "y", "_": "shift-slash",
            ":": "shift-dot", ".": "dot", "/": "shift-7"}

def main():
    tmp = tempfile.mkdtemp()
    sock_path, dump = os.path.join(tmp, "mon.sock"), os.path.join(tmp, "vga.bin")
    q = subprocess.Popen(["qemu-system-x86_64", "-drive", f"format=raw,file={IMG}", "-cpu", "max",
                          "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
                          "-display", "none", "-serial", "null", "-no-reboot",
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
            mon(f'pmemsave 0xb8000 4000 "{dump}"'); time.sleep(1)
            d = open(dump, "rb").read()
            snaps.extend("".join(chr(d[(r*80+c)*2]) if 32 <= d[(r*80+c)*2] < 127 else "." for c in range(80)).rstrip()
                         for r in range(25))
        def wait_for(text, seconds):
            end = time.time() + seconds
            while time.time() < end:
                n = len(snaps); snap()
                if any(text in r for r in snaps[n:]): return True
                time.sleep(1)
            return False

        time.sleep(3)
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
    screen = "".join(snaps)  # rows joined: long messages wrap at 80 columns
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
