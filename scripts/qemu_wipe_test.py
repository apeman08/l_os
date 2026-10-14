#!/usr/bin/env python3
"""Memory-dump acceptance test for the emergency wipe (M2, spec §7/§8.6).

Boots the real kernel (`cargo build --features wipe-test`) in QEMU/UEFI from
the image attached as a read-only USB stick (`qemu-xhci`, `usb-storage`
id=stick0, bootindex=0; `--boot ide` boots the same image from IDE instead
and keeps the stick attached, the spec §7.3 fallback, not needed per the
spike), lets it arm the wipe (`wipe-test: armed`) and the stick watch
(`wipe-test: watch armed port=N`), types a random sentinel into the shell
over PS/2 (no usb-kbd is attached, so QMP send-key goes to PS/2), triggers
the wipe, waits for the QMP SHUTDOWN event and then inspects the guest: an
ELF core from `dump-guest-memory`, a `screendump` and the serial log.

Triggers: `hotkey` = Ctrl+Alt+Backspace via send-key; `unplug` = QMP
`device_del stick0` (the boot stick leaves its root port).

Checks (all are evaluated, every failure is listed):
  unplug_triggers_wipe       (--trigger unplug only) the stick watch was
                             armed, and after device_del stick0 a SHUTDOWN
                             event came within 60 s and the WIPE-REPORT says
                             trigger=unplug (the only end-to-end proof that
                             StickRemoved fires on the real stick port)
  wipe_report_present        exactly one WIPE-REPORT line, trigger=<--trigger>, plan
                             ready (`wipe-test: armed` seen), total_us > 0, all
                             stage times present and non-decreasing, and
                             sum(usable) <= bytes <= 2*sum(usable). The degraded
                             wipe (no plan) also prints a report, but with
                             bytes = key slots only; the bytes bounds reject it.
                             (bytes counts bytes written: user frames count
                             twice, see Exec.bytes in src/wipe.rs; the upper
                             bound 2x covers that.)
  poweroff_not_reset         QMP SHUTDOWN with reason guest-shutdown, no RESET
  key_slot_zero_after_wipe   slot was non-zero (pmemsave) before, zero in the dump
  usable_ram_zero_after_wipe every byte of every `wipe-test: usable` range is
                             zero in the dump (the dump also has VGA/flash
                             PT_LOADs; only the usable ranges are evaluated,
                             by p_paddr; a range not covered by the dump fails)
  sentinel_absent_from_dump  the 24-char sentinel and the first 32 key bytes
                             occur nowhere in the whole dump
  screen_black_after_wipe    screendump has only zero pixels
  serial_log_has_no_sentinel sentinel / key prefix not in the serial log
Positive controls (INFRA failure, exit 2, if they do not hold, so that an
absent sentinel is not just a typing failure): before the trigger the sentinel
is in a pre-trigger dump, the key slot is non-zero and the screen is not black.

Durations: WIPE-REPORT total_us runs up to KernelZeroed only (a lower bound
of the wipe); the host time QMP command -> SHUTDOWN event is printed too.
`--measure 256M,1G,4G` runs both triggers at each size (one build) and prints
a table with the host CPU, QEMU version and accelerator.

Usage: python3 -I scripts/qemu_wipe_test.py --trigger hotkey|unplug
           [--mem 256M] [--boot usb|ide] [--keep-dump]
       python3 -I scripts/qemu_wipe_test.py --measure 256M,1G,4G [--boot usb|ide]
Exit codes: 0 all checks pass, 1 a check failed, 2 infrastructure error
(build, QEMU, timeout, positive control).
"""
import argparse, importlib.util, json, mmap, os, re, secrets, shutil, socket, string
import struct, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def _load_smoke():
    sys.dont_write_bytecode = True  # no __pycache__ next to the helper
    path = os.path.join(ROOT, "scripts", "qemu_shell_smoke.py")
    spec = importlib.util.spec_from_file_location("qemu_shell_smoke", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

class Infra(Exception):
    """Infrastructure problem (not a failed check): exit code 2."""

def parse_size(s):
    m = re.fullmatch(r"(\d+)([MG])", s)
    if not m: raise argparse.ArgumentTypeError("--mem must look like 256M or 1G")
    return int(m.group(1)) * (1 << (20 if m.group(2) == "M" else 30))

class Qmp:
    def __init__(self, path, timeout=30):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.settimeout(timeout)
        self.s.connect(path)
        self.buf = b""
        self.events = []  # (monotonic time, name, data)
        json.loads(self._line())  # greeting
        self.cmd("qmp_capabilities")

    def _line(self):
        while b"\n" not in self.buf:
            chunk = self.s.recv(65536)
            if not chunk: raise Infra("QMP connection closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return line

    def _take(self, msg):
        if "event" in msg:
            self.events.append((time.monotonic(), msg["event"], msg.get("data", {})))
            return True
        return False

    def cmd(self, name, timeout=30, **args):
        self.s.settimeout(timeout)
        self.s.sendall(json.dumps({"execute": name, "arguments": args}).encode() + b"\n")
        while True:
            msg = json.loads(self._line())
            if self._take(msg): continue
            if "error" in msg: raise Infra(f"QMP {name}: {msg['error']}")
            return msg.get("return")

    def wait_event(self, name, timeout):
        """Time of the first event `name` (already seen or new), else None."""
        deadline = time.monotonic() + timeout
        while True:
            for t, n, d in self.events:
                if n == name: return t, d
            left = deadline - time.monotonic()
            if left <= 0: return None
            self.s.settimeout(left)
            try:
                self._take(json.loads(self._line()))
            except socket.timeout:
                return None

def serial_text(path):
    try: return open(path, "rb").read().decode("latin-1")
    except OSError: return ""

def wait_serial(path, needle, timeout, q):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        t = serial_text(path)
        if needle in t or "wipe-test: arm failed" in t: return t
        if q.poll() is not None: raise Infra("QEMU exited early")
        time.sleep(0.2)
    raise Infra(f"timeout waiting for {needle!r} on serial")

def elf_segments(mm):
    """PT_LOAD segments of the dump (ELF64): [(paddr, file offset, filesz)]."""
    if mm[:4] != b"\x7fELF" or mm[4] != 2: raise Infra("dump is not an ELF64 file")
    phoff, = struct.unpack_from("<Q", mm, 0x20)
    phentsize, phnum = struct.unpack_from("<HH", mm, 0x36)
    segs = []
    for i in range(phnum):
        p_type, _, p_offset, _, p_paddr, p_filesz, _, _ = struct.unpack_from("<IIQQQQQQ", mm, phoff + i * phentsize)
        if p_type == 1: segs.append((p_paddr, p_offset, p_filesz))
    return segs

def read_phys(mm, segs, start, end):
    """Bytes of physical [start, end) from the dump, or None if not fully covered
    by a single PT_LOAD."""
    for paddr, off, size in segs:
        if paddr <= start and end <= paddr + size:
            return mm[off + start - paddr:off + end - paddr]
    return None

def all_zero(mm, segs, start, end, step=1 << 24):
    """True/False, or None when [start, end) is not in the dump."""
    for a in range(start, end, step):
        b = min(a + step, end)
        data = read_phys(mm, segs, a, b)
        if data is None: return None
        if data.count(0) != len(data): return False
    return True

REPORT_KEYS = ["keys_us", "user_us", "allocated_us", "usable_us", "kernel_us", "total_us", "bytes"]

def parse_serial(text):
    usable = [(int(a, 16), int(b, 16)) for a, b in re.findall(r"wipe-test: usable (0x[0-9a-f]+)-(0x[0-9a-f]+)", text)]
    m = re.search(r"wipe-test: key-slot phys=(0x[0-9a-f]+) len=(\d+)", text)
    slot = (int(m.group(1), 16), int(m.group(2))) if m else None
    reports = [l.strip() for l in text.splitlines() if "WIPE-REPORT" in l]
    return usable, slot, reports

# --trigger value -> WIPE-REPORT trigger= value (l_core::wipe::Trigger::as_str;
# "controller" = ControllerLost has no QEMU trigger here).
REPORT_TRIGGER = {"hotkey": "hotkey", "unplug": "unplug"}

def check_report(reports, armed, usable, want="hotkey"):
    """Returns (ok, message, fields)."""
    if len(reports) != 1: return False, f"{len(reports)} WIPE-REPORT lines (want exactly 1)", {}
    line = reports[0]
    f = dict(re.findall(r"(\w+)=(\S+)", line))
    if f.get("trigger") != want: return False, f"trigger is {f.get('trigger')!r} (want {want!r}): {line}", f
    if not armed: return False, "no 'wipe-test: armed' marker (degraded wipe): " + line, f
    try: v = {k: int(f[k]) for k in REPORT_KEYS}
    except (KeyError, ValueError): return False, "report lacks numeric stage fields: " + line, f
    if v["total_us"] <= 0: return False, "total_us is 0 (TSC uncalibrated?): " + line, f
    seq = [v[k] for k in REPORT_KEYS[:6]]
    if seq != sorted(seq): return False, "stage times not non-decreasing: " + line, f
    total = sum(e - s for s, e in usable)
    if not total <= v["bytes"] <= 2 * total:
        return False, f"bytes={v['bytes']} outside [{total}, {2 * total}] (sum of usable ranges {total}): " + line, f
    return True, line, f

UNPLUG_TIMEOUT = 60  # s, device_del stick0 -> SHUTDOWN (plan Task 24)

def host_env():
    """Host CPU model, QEMU version line, accelerator (for the measurements)."""
    cpu = "?"
    try:
        for l in open("/proc/cpuinfo"):
            if l.startswith("model name"): cpu = l.split(":", 1)[1].strip(); break
    except OSError: pass
    try: ver = subprocess.run([_load_smoke().tool("--print-qemu"), "--version"], stdout=subprocess.PIPE, text=True).stdout.splitlines()[0]
    except (OSError, IndexError, subprocess.SubprocessError): ver = "?"
    return cpu, ver

def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--trigger", choices=["hotkey", "unplug"])
    ap.add_argument("--mem", type=parse_size, default=parse_size("256M"))
    ap.add_argument("--boot", choices=["usb", "ide"], default="usb")
    ap.add_argument("--keep-dump", action="store_true")
    ap.add_argument("--measure", metavar="SIZES", help="comma-separated sizes, e.g. 256M,1G,4G: both triggers each")
    args = ap.parse_args()
    if args.measure:
        try: sizes = [parse_size(x) for x in args.measure.split(",")]
        except argparse.ArgumentTypeError as e: ap.error(str(e))
        runs = [(m, t) for m in sizes for t in ("hotkey", "unplug")]
    elif args.trigger:
        runs = [(args.mem, args.trigger)]
    else:
        ap.error("--trigger or --measure is required")
    try:
        _load_smoke().build_image("wipe-test")
    except (OSError, subprocess.SubprocessError) as e:
        print(f"INFRA FAIL: build: {type(e).__name__}: {e}")
        return 2
    codes, rows = [], []
    for mem, trigger in runs:
        if len(runs) > 1: print(f"=== --trigger {trigger} --mem {mem >> 20}M --boot {args.boot} ===", flush=True)
        code, row = run_one(args, mem, trigger)
        codes.append(code); rows.append((mem, trigger, code, row))
    if len(runs) > 1:
        cpu, ver = host_env()
        print(f"\nhost CPU: {cpu}; {os.cpu_count()} logical CPUs; {ver}; accelerator TCG (no -accel/-enable-kvm)")
        print("| RAM | trigger | result | total_us | keys_us | user_us | usable_us | bytes | host command->SHUTDOWN |")
        print("|---|---|---|---|---|---|---|---|---|")
        for mem, trigger, code, r in rows:
            host = f"{r['host_s']:.3f} s" if r.get("host_s") is not None else "-"
            print(f"| {mem >> 20} MiB | {trigger} | {['PASS', 'FAIL', 'INFRA'][code]} | " + " | ".join(
                str(r.get(k, "-")) for k in ("total_us", "keys_us", "user_us", "usable_us", "bytes")) + f" | {host} |")
    return max(codes)

def run_one(args, mem, trigger):
    """(exit code, measurement fields) of one boot + trigger."""
    tmp = tempfile.mkdtemp(prefix="wipe-test-")
    row = {}
    try:
        return run(args, mem, trigger, tmp, row), row
    except Infra as e:
        print(f"INFRA FAIL: {e}")
        return 2, row
    except (OSError, subprocess.SubprocessError, socket.timeout, ValueError) as e:
        print(f"INFRA FAIL: {type(e).__name__}: {e}")
        return 2, row
    finally:
        if args.keep_dump: print(f"kept temp dir {tmp}")
        else: shutil.rmtree(tmp, ignore_errors=True)

def qemu_cmd(sm, args, mem, serial, sock):
    img = sm.IMG.replace(",", ",,")
    # The same image as a read-only USB stick on the xHCI (tools/image
    # usb_test_args, without usb-kbd: send-key must reach PS/2). OVMF boots
    # from it (spike note, question 2); --boot ide boots from an IDE copy
    # (snapshot=on: ide-hd refuses readonly=on) and keeps the stick attached.
    stick = ["-device", "qemu-xhci,id=xhci",
             "-drive", f"if=none,id=stick,format=raw,readonly=on,file={img}",
             "-device", "usb-storage,bus=xhci.0,drive=stick,id=stick0" + (",bootindex=0" if args.boot == "usb" else "")]
    disk = [] if args.boot == "usb" else [
        "-drive", f"if=none,id=disk0,format=raw,file={img},snapshot=on", "-device", "ide-hd,drive=disk0,bootindex=0"]
    return [sm.tool("--print-qemu"), *disk, *stick, *sm.tool("--print-qemu-firmware-args").splitlines(),
        "-m", f"{mem >> 20}M", "-cpu", "max", "-vga", "none", "-device", "VGA,xres=1024,yres=768",
        "-display", "none", "-serial", f"file:{serial}", "-qmp", f"unix:{sock},server=on,wait=off",
        "-action", "reboot=shutdown,shutdown=pause"]

def run(args, mem, trigger, tmp, row):
    sm = _load_smoke()
    sock, serial = os.path.join(tmp, "qmp.sock"), os.path.join(tmp, "serial.log")
    keyfile, pre_dump, dump = (os.path.join(tmp, n) for n in ("key.bin", "pre.core", "post.core"))
    pre_ppm, post_ppm = os.path.join(tmp, "pre.ppm"), os.path.join(tmp, "post.ppm")
    q = subprocess.Popen(qemu_cmd(sm, args, mem, serial, sock), stdin=subprocess.DEVNULL)
    try:
        for _ in range(100):
            if os.path.exists(sock): break
            if q.poll() is not None: raise Infra("QEMU exited at start")
            time.sleep(0.1)
        qmp = Qmp(sock)
        text = wait_serial(serial, "wipe-test: armed", 180, q)
        usable, slot, _ = parse_serial(text)
        armed = "wipe-test: armed" in text
        if not armed: raise Infra("kernel did not arm the wipe: " + text[-300:])
        if not usable or slot is None: raise Infra("missing wipe-test: usable / key-slot lines")
        print(f"boot ok ({args.boot}, {mem >> 20} MiB): {len(usable)} usable ranges, {sum(e - s for s, e in usable)} bytes, "
              f"max {max(e for _, e in usable):#x}, key slot at {slot[0]:#x}")
        # The stick watch arms after USB enumeration, identification and seal
        # (src/main.rs). Wait for it before typing, too: keys typed during the
        # enumeration are not lost (PS/2), but the shell is not up yet.
        deadline = time.monotonic() + 120
        while "wipe-test: watch armed" not in (text := serial_text(serial)) and time.monotonic() < deadline:
            if q.poll() is not None: raise Infra("QEMU exited early")
            time.sleep(0.2)
        m = re.search(r"wipe-test: watch armed port=(\d+)", text)
        watch_port = int(m.group(1)) if m else None
        print(f"stick watch: {'armed on root port %d' % watch_port if m else 'NOT armed (no marker within 120 s)'}")

        # Positive controls before the trigger.
        qmp.cmd("pmemsave", val=slot[0], size=slot[1], filename=keyfile)
        key = open(keyfile, "rb").read()
        if len(key) != slot[1] or key.count(0) == len(key): raise Infra("key slot is zero before the wipe")
        sentinel = "".join(secrets.choice(string.ascii_lowercase) for _ in range(24)).encode()
        keynames = sm.KEYNAMES
        def send(names, hold=60):
            qmp.cmd("send-key", keys=[{"type": "qcode", "data": n} for n in names], **{"hold-time": hold})
        seen = False
        for attempt in range(4):
            time.sleep(3)
            for ch in "echo " + sentinel.decode() + "\n":
                send([keynames.get(ch, ch)]); time.sleep(0.08)
            time.sleep(1.5)
            qmp.cmd("dump-guest-memory", timeout=300, paging=False, protocol=f"file:{pre_dump}")
            with open(pre_dump, "rb") as fh, mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                seen = mm.find(sentinel) >= 0
            os.unlink(pre_dump)
            if seen: break
        if not seen: raise Infra("positive control failed: sentinel not in guest memory before the trigger")
        qmp.cmd("screendump", filename=pre_ppm)
        _, _, px = sm.parse_ppm(open(pre_ppm, "rb").read())
        if px.count(0) == len(px): raise Infra("positive control failed: screen is black before the trigger")
        print(f"positive controls ok (sentinel {sentinel.decode()} in guest memory, key slot non-zero, screen not black)")

        results = []
        def result(name, ok, msg=""):
            results.append((name, ok, msg)); show = msg and (not ok or name in ("poweroff_not_reset", "wipe_report_present", "unplug_triggers_wipe"))
            print(f"check {name}: {'OK' if ok else 'FAIL'}" + (f" ({msg})" if show else ""))

        # Trigger.
        if trigger == "hotkey":
            t0 = time.monotonic()
            send(["ctrl", "alt", "backspace"], hold=100)
            ev = qmp.wait_event("SHUTDOWN", 120 + 30 * (mem >> 30))
        else:
            t0 = time.monotonic()
            qmp.cmd("device_del", id="stick0")
            ev = qmp.wait_event("SHUTDOWN", UNPLUG_TIMEOUT)
        if ev is None:
            result("poweroff_not_reset", False, "no SHUTDOWN event (wipe did not finish)")
            host_s = None
        else:
            host_s = ev[0] - t0
            reasons = [d.get("reason") for _, n, d in qmp.events if n == "SHUTDOWN"]
            resets = [n for _, n, _ in qmp.events if n == "RESET"]
            result("poweroff_not_reset", reasons == ["guest-shutdown"] and not resets,
                   f"SHUTDOWN reasons {reasons}, RESET events {len(resets)}")
        sert = serial_text(serial)
        _, _, reports = parse_serial(sert)
        ok, msg, fields = check_report(reports, armed, usable, REPORT_TRIGGER[trigger])
        if trigger == "unplug":
            deleted = any(n == "DEVICE_DELETED" and d.get("device") == "stick0" for _, n, d in qmp.events)
            why = []
            if watch_port is None: why.append("stick watch never armed")
            if ev is None: why.append(f"no SHUTDOWN within {UNPLUG_TIMEOUT} s of device_del stick0")
            if fields.get("trigger") != "unplug": why.append(f"WIPE-REPORT trigger={fields.get('trigger')!r}")
            result("unplug_triggers_wipe", not why, "; ".join(why) if why else
                   f"port {watch_port}, DEVICE_DELETED {'seen' if deleted else 'not seen'}, SHUTDOWN after {host_s:.3f} s")
        result("wipe_report_present", ok, msg)

        have_dump = False
        if ev is not None:
            qmp.cmd("screendump", filename=post_ppm)
            qmp.cmd("dump-guest-memory", timeout=600, paging=False, protocol=f"file:{dump}")
            have_dump = True
        if have_dump:
            _, _, px = sm.parse_ppm(open(post_ppm, "rb").read())
            result("screen_black_after_wipe", px.count(0) == len(px), f"{len(px) - px.count(0)} non-zero bytes")
            with open(dump, "rb") as fh, mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                segs = elf_segments(mm)
                z = all_zero(mm, segs, slot[0], slot[0] + slot[1])
                result("key_slot_zero_after_wipe", z is True, "not in dump" if z is None else "")
                bad = []
                for s, e in usable:
                    z = all_zero(mm, segs, s, e)
                    if z is not True: bad.append(f"{s:#x}-{e:#x}: {'not in dump' if z is None else 'NON-ZERO'}")
                result("usable_ram_zero_after_wipe", not bad, "; ".join(bad))
                found = [n for n, needle in (("sentinel", sentinel), ("key prefix", key[:32])) if mm.find(needle) >= 0]
                result("sentinel_absent_from_dump", not found, "found in dump: " + ", ".join(found))
            os.unlink(dump)  # up to ~4.3 GB at 4G; free it before the next run
        else:
            for n in ("screen_black_after_wipe", "key_slot_zero_after_wipe", "usable_ram_zero_after_wipe", "sentinel_absent_from_dump"):
                result(n, False, "no dump (no SHUTDOWN)")
        leaks = [n for n, needle in (("sentinel", sentinel), ("key prefix", key[:32])) if needle in sert.encode("latin-1")]
        result("serial_log_has_no_sentinel", not leaks, "in serial log: " + ", ".join(leaks))

        row.update({k: fields[k] for k in REPORT_KEYS if k in fields})
        row["host_s"] = host_s
        if fields.get("total_us"):
            print(f"wipe duration: WIPE-REPORT total_us={fields.get('total_us')} (lower bound, up to KernelZeroed), "
                  f"keys_us={fields.get('keys_us')} user_us={fields.get('user_us')} usable_us={fields.get('usable_us')}")
        if host_s is not None:
            what = "send-key" if trigger == "hotkey" else "device_del stick0"
            print(f"wipe duration: host time QMP {what} -> SHUTDOWN = {host_s:.3f} s")
        failed = [n for n, ok, _ in results if not ok]
        print("PASS" if not failed else "FAIL: " + ", ".join(failed))
        return 0 if not failed else 1
    finally:
        if q.poll() is None:
            q.kill()
        q.wait()

if __name__ == "__main__":
    sys.exit(main())
