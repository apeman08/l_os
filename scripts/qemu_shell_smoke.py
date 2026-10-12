#!/usr/bin/env python3
"""Boot the real kernel image in QEMU (UEFI), type commands via the QEMU monitor
and assert on the console output. This exercises the actual kernel, not host code.

Two checks:
1. Text: the kernel mirrors every console write to COM1 (cargo feature
   `serial-console`, on by default), which QEMU writes to a file here. The text
   is rendered like a terminal (backspace erases, ANSI sequences are dropped)
   and searched for the expected strings.
2. Pixels: after `clear`, `version` and `help`, a QEMU `screendump` of the
   framebuffer console is compared row by row, pixel-exactly, with the same
   text rendered on the host by `l_core::render` (helper
   crates/l_core/examples/render_text.rs, built here with `cargo +stable` from
   a neutral directory, like scripts/host_tests.sh does). Every text row of the
   screen is checked: the expected lines, then blank rows.

Usage: scripts/build_image.sh && python3 -I scripts/qemu_shell_smoke.py
Needs: qemu-system-x86_64, OVMF (see scripts/ovmf_path.sh; OVMF_FD overrides),
and a stable Rust toolchain for the host helper.
Characters are mapped to keys for the kernel's German layout.
"""
import os, re, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IMG = os.path.join(ROOT, "target/x86_64-l_os/debug/l_os-uefi.img")
# The kernel uses the German layout (De105Key): the US y/z keys are swapped.
KEYNAMES = {" ": "spc", "\n": "ret", "\b": "backspace", "y": "z", "z": "y", "_": "shift-slash", ":": "shift-dot", ".": "dot", "/": "shift-7"}
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

GLYPH_W, GLYPH_H = 8, 16

def build_render_helper(tmp):
    """Build the host helper; run cargo from a neutral directory so the kernel's
    .cargo/config.toml (target, build-std) does not apply."""
    subprocess.run(["cargo", "+stable", "build", "--quiet", "--example", "render_text",
                    "--manifest-path", os.path.join(ROOT, "crates/l_core/Cargo.toml"),
                    "--target-dir", os.path.join(ROOT, "target/host-tests")], cwd=tmp, check=True)
    return os.path.join(ROOT, "target/host-tests/debug/examples/render_text")

def parse_ppm(data):
    """Binary PPM (P6, maxval 255) -> (width, height, RGB bytes)."""
    fields, pos = [], 0
    while len(fields) < 4:
        while data[pos:pos + 1].isspace(): pos += 1
        if data[pos:pos + 1] == b"#":
            pos = data.index(b"\n", pos)
            continue
        end = pos
        while not data[end:end + 1].isspace(): end += 1
        fields.append(data[pos:end]); pos = end
    if fields[0] != b"P6" or fields[3] != b"255":
        raise ValueError(f"unexpected PPM header {fields}")
    w, h = int(fields[1]), int(fields[2])
    pixels = data[pos + 1:pos + 1 + w * h * 3]
    if len(pixels) != w * h * 3:
        raise ValueError("truncated PPM")
    return w, h, pixels

def compare_screen(helper, dump_path, lines):
    """Pixel-compare every text row of the screendump with `lines` (then blank rows).
    Returns a list of mismatch descriptions (empty = all rows equal)."""
    w, h, screen = parse_ppm(open(dump_path, "rb").read())
    cols, rows = w // GLYPH_W, h // GLYPH_H
    if len(lines) > rows:
        return [f"{len(lines)} expected lines do not fit on {rows} rows"]
    expected = lines + [""] * (rows - len(lines))
    out = subprocess.run([helper, str(cols)], input="\n".join(expected).encode() + b"\n",
                         capture_output=True, check=True).stdout
    ew, eh, want = parse_ppm(out)
    assert (ew, eh) == (cols * GLYPH_W, rows * GLYPH_H), (ew, eh)
    used = cols * GLYPH_W * 3
    bad = []
    for row, text in enumerate(expected):
        same = all(screen[y * w * 3:y * w * 3 + used] == want[y * ew * 3:y * ew * 3 + used]
                   for y in range(row * GLYPH_H, (row + 1) * GLYPH_H))
        if not same:
            bad.append(f"row {row}: expected {text!r}")
    return bad

def main():
    tmp = tempfile.mkdtemp()
    helper = build_render_helper(tmp)
    dump = os.path.join(tmp, "screen.ppm")
    sock_path, serial = os.path.join(tmp, "mon.sock"), os.path.join(tmp, "serial.log")
    q = subprocess.Popen(["qemu-system-x86_64", "-drive", f"if=none,id=disk0,format=raw,file={IMG},snapshot=on",
                          # bootindex: OVMF then connects only the disk and leaves the NIC alone.
                          "-device", "ide-hd,drive=disk0,bootindex=0", "-bios", ovmf(),
                          "-vga", "none", "-device", "VGA,xres=1024,yres=768",
                          "-cpu", "max", "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0,disable-modern=on", "-display", "none", "-serial", f"file:{serial}", "-no-reboot",
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
        snaps = []
        def snap():
            """Take the console output so far (the serial mirror holds all of it)."""
            time.sleep(1)
            snaps[:] = [r.rstrip() for r in render(open(serial, "rb").read())]
        # UEFI firmware + bootloader take a few seconds; wait for the prompt.
        for _ in range(60):
            snap()
            if any("l_os>" in r for r in snaps): break
        else:
            print("FAIL: no shell prompt on the serial console"); sys.exit(1)
        # Pixel check: a cleared screen with the prompt and the output of
        # `version` and `help`, compared with the host rendering of the same lines.
        type_("clear\n"); type_("version\n"); type_("help\n"); snap()
        raw = open(serial, "rb").read()
        clear_at = raw.rfind(b"\x1b[2J")
        if clear_at < 0:
            print("FAIL: `clear` did not reach the console"); sys.exit(1)
        screen_lines = render(raw[clear_at:])
        for want in ["l_os> version", "Larp OS ", "l_os> help", "Commands:", "  clear     clear the screen", "l_os> "]:
            if not any(r.startswith(want) for r in screen_lines):
                print(f"FAIL: {want!r} missing from the console text after `clear`:", screen_lines); sys.exit(1)
        mon(f"screendump {dump}")
        for _ in range(50):
            if os.path.exists(dump) and os.path.getsize(dump) > 0: time.sleep(0.2); break
            time.sleep(0.1)
        pixel_errors = compare_screen(helper, dump, screen_lines)
        if pixel_errors:
            print("FAIL: screendump differs from the host rendering:"); print("\n".join(pixel_errors)); sys.exit(1)
        print(f"pixel check: {len(screen_lines)} text rows + blank rest match the host rendering:")
        print("\n".join("  | " + r for r in screen_lines))
        type_("help\n"); type_("echo hix\b\n"); type_("bogus\n"); type_("help me\n"); snap()
        type_("uptime\n"); type_("security\n"); snap()
        type_("run hello\n"); type_("run hello_rs\n"); type_("run evil\n"); snap()
        type_("run spin\n"); time.sleep(2); type_("run nope\n"); snap()
        # Network management on the trusted path (nothing is ever sent here).
        type_("clear\n"); type_("net status\n"); type_("net devices\n"); type_("net permissions\n"); snap()
        type_("clear\n"); type_("net request 10.0.2.2:18080 60\n"); time.sleep(0.5); type_("n"); time.sleep(0.5); snap()
        type_("net request 10.0.2.2:18080 60\n"); time.sleep(0.5); type_("y"); time.sleep(0.5)
        type_("net permissions\n"); type_("net revoke 10.0.2.2:18080\n"); type_("net permissions\n"); snap()
    finally:
        q.terminate(); q.wait()
    screen = "\n".join(snaps)
    print("\n".join(snaps[-25:]))
    expected = ["l_os> echo hi", "\nhi\n", "unknown command: bogus", "help: unexpected arguments",
                "ticks (~", "denied by default", "ring 3", "SMEP on, SMAP on", "hello from ring 3",
                "[process exited with code 18]", "hello from Rust in ring 3", "[process exited with code 0]",
                "shell running in ring 3", "illegal memory access",
                "exceeded its time budget", "unknown program 'nope'",
                "state: DOWN - never initialized", "1af4:1000 ethernet", "no authorizations: every network operation is denied",
                "NOT AUTHORIZED. Nothing changed.", "AUTHORIZED (enforced by the kernel): 10.0.2.2:18080 for 60 s",
                "10.0.2.2:18080 for", "revoked 10.0.2.2:18080"]
    missing = [e for e in expected if e not in screen]
    if missing:
        print("---- all captured console output ----")
        print("\n".join(r for r in snaps if r.strip()))
        print("FAIL, missing:", missing); sys.exit(1)
    print("OK")

main()
