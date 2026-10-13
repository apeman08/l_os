#!/usr/bin/env python3
"""Boot the real kernel image in QEMU (UEFI), type commands via the QEMU monitor
and assert on the console output. This exercises the actual kernel, not host code.

Checks:
1. Text: the kernel mirrors every console write to COM1 (cargo feature
   `serial-console`, on by default), which QEMU writes to a file here. The text
   is rendered like a terminal (backspace erases, ANSI sequences are dropped)
   and searched for the expected strings.
2. Pixels: after `clear`, `version` and `help`, a QEMU `screendump` of the
   framebuffer console is compared row by row, pixel-exactly, with the same
   text rendered on the host by `l_core::render` (helper
   crates/l_core/examples/render_text.rs, built here with `cargo +stable` from
   a neutral directory, like scripts/host_tests.sh does). Every text row of the
   screen is checked: the Aurora status bar (row 0), the expected lines, blank
   rows, and the hint bar (last row); the helper draws the bars with the same
   `l_core::widgets` code in the final AURORA colours. The same is done at the
   end for a screen that output has filled (prompt on the last text row).
3. `shell_screen_has_status_and_at_most_three_hints`: the first and last rows
   of the screendump are read back as text (each 8x16 cell matched against the
   font's glyphs, rendered by the helper): row 0 must say "Larp OS" and
   "amnesisch: nein", the last row must hold at most three hints.

Usage: python3 -I scripts/qemu_shell_smoke.py [--screenshot OUT.png]
It builds the kernel (`cargo build`) and its UEFI image first (host tool
l_os_image, see tools/image).
With --screenshot, the final screendump (the real kernel screen) is saved as PNG.
Needs: qemu-system-x86_64, OVMF (found like the cargo runner does: OVMF_FD
overrides, see tools/image/src/lib.rs), a stable Rust toolchain for the host
helper, and a Unix host (the QEMU monitor is a Unix socket).
Characters are mapped to keys for the kernel's German layout.
"""
import os, re, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# The kernel uses the German layout (De105Key): the US y/z keys are swapped.
KEYNAMES = {" ": "spc", "\n": "ret", "\b": "backspace", "y": "z", "z": "y", "_": "shift-slash", ":": "shift-dot", ".": "dot", "/": "shift-7"}
ANSI = re.compile(r"\x1b(\[[0-9;=?]*[A-Za-z]|[()][0-9A-Za-z])")

TOOLS = os.path.join(ROOT, "target", "l_os-tools", "debug")
EXE = ".exe" if os.name == "nt" else ""
KERNEL = os.path.join(ROOT, "target", "x86_64-l_os", "debug", "l_os")
IMG = KERNEL + "-uefi.img"

def tool(*args):
    """Run the host tool l_os_image (built by the repository's build.rs into
    target/l_os-tools); its stdout, stripped. QEMU and firmware are found the
    same way the cargo runner finds them (OVMF_FD overrides the firmware)."""
    return subprocess.run([os.path.join(TOOLS, "l_os_image" + EXE), *args], check=True,
                          stdout=subprocess.PIPE, text=True).stdout.strip()

def build_image():
    """`cargo build` (whose build.rs also builds the host tools), then wrap the
    kernel into the UEFI image IMG, like the cargo runner does."""
    subprocess.run(["cargo", "build"], cwd=ROOT, check=True)
    tool(KERNEL, IMG)

def render(raw):
    """Serial bytes -> screen lines (backspace erases, escape sequences dropped)."""
    lines = [""]
    for ch in ANSI.sub("", raw.decode("latin-1")).replace("\r", ""):
        if ch == "\n": lines.append("")
        elif ch == "\b": lines[-1] = lines[-1][:-1]
        else: lines[-1] += ch if 32 <= ord(ch) < 127 else "."
    return lines

GLYPH_W, GLYPH_H = 8, 16
# The chrome the kernel draws (src/aurora.rs); the helper renders the expected
# bars from these, the OCR check reads the real screen independently.
STATUS = ("Larp OS", "amnesisch: nein")
HINTS = [("Enter", "ausführen"), ("Backspace", "löschen"), ("help", "Befehle")]
# Keys that do not exist in the shell (yet) and must not be advertised.
NOT_IMPLEMENTED = ["Tab", "Strg", "Ctrl", "Palette"]
OCR_CHARS = "".join(chr(c) for c in range(0x20, 0x7f)) + "äöüÄÖÜß"

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

def chrome_args():
    args = ["--status", *STATUS]
    for key, desc in HINTS:
        args += ["--hint", key, desc]
    return args

def compare_screen(helper, dump_path, lines):
    """Pixel-compare every text row of the screendump: the status bar, `lines` in
    the text region (then blank rows), the hint bar.
    Returns a list of mismatch descriptions (empty = all rows equal)."""
    w, h, screen = parse_ppm(open(dump_path, "rb").read())
    cols, rows = w // GLYPH_W, h // GLYPH_H
    region = rows - 2
    if len(lines) > region:
        return [f"{len(lines)} expected lines do not fit on {region} text rows"]
    expected = lines + [""] * (region - len(lines))
    out = subprocess.run([helper, str(cols), *chrome_args()], input="\n".join(expected).encode() + b"\n",
                         capture_output=True, check=True).stdout
    ew, eh, want = parse_ppm(out)
    assert (ew, eh) == (cols * GLYPH_W, rows * GLYPH_H), (ew, eh)
    used = cols * GLYPH_W * 3
    labels = [f"status bar {STATUS}"] + [repr(t) for t in expected] + [f"hint bar {HINTS}"]
    bad = []
    for row, label in enumerate(labels):
        same = all(screen[y * w * 3:y * w * 3 + used] == want[y * ew * 3:y * ew * 3 + used]
                   for y in range(row * GLYPH_H, (row + 1) * GLYPH_H))
        if not same:
            bad.append(f"row {row}: expected {label}")
    return bad

def glyph_masks(helper):
    """Font bitmap of every OCR candidate, as the helper renders it: {mask: char}."""
    out = subprocess.run([helper, str(len(OCR_CHARS))], input=OCR_CHARS.encode() + b"\n",
                         capture_output=True, check=True).stdout
    w, h, px = parse_ppm(out)
    masks = {}
    for i, ch in enumerate(OCR_CHARS):
        cell = [px[(y * w + i * GLYPH_W + x) * 3:(y * w + i * GLYPH_W + x) * 3 + 3]
                for y in range(GLYPH_H) for x in range(GLYPH_W)]
        bg = max(set(cell), key=cell.count)
        masks.setdefault(tuple(p != bg for p in cell), ch)
    return masks

def read_row(dump_path, row, masks):
    """Text of one screen row, read from the pixels: each cell's background is its
    most common colour, the other pixels form a glyph that is looked up in the font."""
    w, h, px = parse_ppm(open(dump_path, "rb").read())
    text = ""
    for col in range(w // GLYPH_W):
        cell = [px[((row * GLYPH_H + y) * w + col * GLYPH_W + x) * 3:((row * GLYPH_H + y) * w + col * GLYPH_W + x) * 3 + 3]
                for y in range(GLYPH_H) for x in range(GLYPH_W)]
        bg = max(set(cell), key=cell.count)
        text += masks.get(tuple(p != bg for p in cell), "?")
    return text

def shell_screen_has_status_and_at_most_three_hints(dump_path, masks):
    """Reads the first and last screen rows back as text. Returns a list of errors."""
    w, h, _ = parse_ppm(open(dump_path, "rb").read())
    top, bottom = read_row(dump_path, 0, masks), read_row(dump_path, h // GLYPH_H - 1, masks)
    print(f"  status row: {top.rstrip()!r}\n  hint row:   {bottom.rstrip()!r}")
    errors = []
    for want in STATUS:
        if want not in top:
            errors.append(f"status row lacks {want!r}: {top.rstrip()!r}")
    # HintBar layout: entries ("key description") separated by two spaces.
    entries = [e for e in re.split(r" {2,}", bottom.strip()) if e]
    if not 1 <= len(entries) <= 3:
        errors.append(f"hint row has {len(entries)} entries (want 1..3): {entries}")
    for word in NOT_IMPLEMENTED:
        if word in bottom:
            errors.append(f"hint row advertises {word!r}, which the shell does not have")
    return errors

def write_png(dump_path, out_path):
    """Screendump (PPM) -> PNG with the standard library only (zlib + struct);
    an indexed-colour PNG when the screen has at most 256 colours (it does: the
    console draws only theme colours), else RGB."""
    import struct, zlib
    w, h, px = parse_ppm(open(dump_path, "rb").read())
    colours = sorted({px[i:i + 3] for i in range(0, len(px), 3)})
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)
    if len(colours) <= 256:
        index = {c: bytes([i]) for i, c in enumerate(colours)}
        raw = b"".join(b"\0" + b"".join(index[px[(y * w + x) * 3:(y * w + x) * 3 + 3]] for x in range(w))
                       for y in range(h))
        ihdr, extra = struct.pack(">IIBBBBB", w, h, 8, 3, 0, 0, 0), chunk(b"PLTE", b"".join(colours))
    else:
        raw = b"".join(b"\0" + px[y * w * 3:(y + 1) * w * 3] for y in range(h))
        ihdr, extra = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0), b""
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + extra + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    open(out_path, "wb").write(png)
    return len(png)

def take_dump(mon, path):
    mon(f"screendump {path}")
    for _ in range(50):
        if os.path.exists(path) and os.path.getsize(path) > 0: time.sleep(0.2); break
        time.sleep(0.1)

def main():
    screenshot = None
    if sys.argv[1:2] == ["--screenshot"] and len(sys.argv) == 3:
        screenshot = sys.argv[2]
    elif len(sys.argv) > 1:
        print("usage: qemu_shell_smoke.py [--screenshot OUT.png]"); sys.exit(2)
    build_image()
    tmp = tempfile.mkdtemp()
    helper = build_render_helper(tmp)
    masks = glyph_masks(helper)
    dump, final_dump = os.path.join(tmp, "screen.ppm"), os.path.join(tmp, "final.ppm")
    sock_path, serial = os.path.join(tmp, "mon.sock"), os.path.join(tmp, "serial.log")
    q = subprocess.Popen([tool("--print-qemu"), "-drive", f"if=none,id=disk0,format=raw,file={IMG},snapshot=on",
                          # bootindex: OVMF then connects only the disk and leaves the NIC alone.
                          "-device", "ide-hd,drive=disk0,bootindex=0", "-bios", tool("--print-firmware"),
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
        take_dump(mon, dump)
        chrome_errors = shell_screen_has_status_and_at_most_three_hints(dump, masks)
        if chrome_errors:
            print("FAIL: shell_screen_has_status_and_at_most_three_hints:"); print("\n".join(chrome_errors)); sys.exit(1)
        print("check shell_screen_has_status_and_at_most_three_hints: OK")
        pixel_errors = compare_screen(helper, dump, screen_lines)
        if pixel_errors:
            print("FAIL: screendump differs from the host rendering:"); print("\n".join(pixel_errors)); sys.exit(1)
        print(f"pixel check: status bar, {len(screen_lines)} text rows + blank rest, hint bar match the host rendering:")
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
        # A screen that output has filled: the prompt sits on the last text row,
        # directly above the hint bar, and the bars are untouched.
        type_("clear\n"); type_("version\n"); type_("security\n"); type_("help\n"); type_("uptime\n"); type_("help\n"); type_("uptime\n"); type_("version\n"); snap()
        raw = open(serial, "rb").read()
        final_lines = render(raw[raw.rfind(b"\x1b[2J"):])
        take_dump(mon, final_dump)
        w, h, _ = parse_ppm(open(final_dump, "rb").read())
        region = h // GLYPH_H - 2
        if len(final_lines) <= region or final_lines[-1] != "l_os> ":
            print(f"FAIL: the output should overflow the {region} text rows and end at the prompt:", final_lines); sys.exit(1)
        last_text_row = read_row(final_dump, region, masks)
        if last_text_row.rstrip() != "l_os>":
            print(f"FAIL: last text row (above the hint bar) is {last_text_row.rstrip()!r}, not the prompt"); sys.exit(1)
        errors = shell_screen_has_status_and_at_most_three_hints(final_dump, masks)
        errors += compare_screen(helper, final_dump, final_lines[-region:])
        if errors:
            print("FAIL: filled screen:"); print("\n".join(errors)); sys.exit(1)
        print(f"filled screen: prompt on text row {region} above the hint bar; all rows match the host rendering")
        if screenshot:
            print(f"screenshot: {screenshot} ({write_png(final_dump, screenshot)} bytes)")
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
