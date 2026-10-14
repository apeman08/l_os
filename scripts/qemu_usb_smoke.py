#!/usr/bin/env python3
"""USB smoke test (M2 Task 22): the real kernel with an xHCI controller, the
boot stick and a USB keyboard, driven through QMP.

QEMU gets `qemu-xhci`, this very image as a read-only `usb-storage` stick
(the kernel identifies and seals it by its boot marker) and `usb-kbd`
(`id=kbd0`), like the `usb_*` kernel tests (tools/image `usb_test_args`).
The kernel boots from the same image on IDE via OVMF, like
scripts/qemu_shell_smoke.py. With `usb-kbd` attached, QMP `send-key` reaches
the USB keyboard, not PS/2 (spike note, question 5); after `device_del kbd0`
it goes to PS/2 again.

Checks:
  usb_keyboard_types_into_shell          text typed with send-key appears as
                                         shell output; `security` says
                                         "usb: tastatur ja" and the report
                                         counter ("tastatur-berichte") is > 0
  stick_read_count_constant_after_seal   `security` shows the stick sealed;
                                         its read count is the same before and
                                         after `run hello` and `run hello_rs`
  keyboard_unplug_does_not_wipe          QMP device_del kbd0 while Ctrl is held
                                         on it; then typing (now PS/2) still
                                         reaches the shell, a PS/2
                                         Alt+Backspace does not wipe (no
                                         stuck Ctrl),
                                         `security` says "usb: tastatur nein",
                                         and there is no SHUTDOWN event

Usage: python3 -I scripts/qemu_usb_smoke.py
Builds with `--features serial-console` (the console is read from COM1).
Exit codes: 0 all checks pass, 1 a check failed, 2 infrastructure error.
"""
import importlib.util, json, os, re, shutil, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _load(name):
    sys.dont_write_bytecode = True  # no __pycache__ next to the helpers
    spec = importlib.util.spec_from_file_location(name, os.path.join(ROOT, "scripts", name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Infra(Exception):
    """Infrastructure problem (not a failed check): exit code 2."""


def main():
    sm = _load("qemu_shell_smoke")
    wt = _load("qemu_wipe_test")
    tmp = tempfile.mkdtemp(prefix="usb-smoke-")
    try:
        return run(sm, wt, tmp)
    except (Infra, wt.Infra) as e:
        print(f"INFRA FAIL: {e}")
        return 2
    except (OSError, subprocess.SubprocessError, socket.timeout, ValueError) as e:
        print(f"INFRA FAIL: {type(e).__name__}: {e}")
        return 2
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def run(sm, wt, tmp):
    sm.build_image("serial-console")
    sock, serial = os.path.join(tmp, "qmp.sock"), os.path.join(tmp, "serial.log")
    img = sm.IMG.replace(",", ",,")
    q = subprocess.Popen([sm.tool("--print-qemu"),
        "-drive", f"if=none,id=disk0,format=raw,file={img},snapshot=on",
        "-device", "ide-hd,drive=disk0,bootindex=0", *sm.tool("--print-qemu-firmware-args").splitlines(),
        "-device", "qemu-xhci,id=xhci",
        "-drive", f"if=none,id=stick,format=raw,readonly=on,file={img}",
        "-device", "usb-storage,bus=xhci.0,drive=stick,id=stick0",
        "-device", "usb-kbd,bus=xhci.0,id=kbd0",
        "-cpu", "max", "-vga", "none", "-device", "VGA,xres=1024,yres=768",
        "-display", "none", "-serial", f"file:{serial}", "-qmp", f"unix:{sock},server=on,wait=off",
        "-action", "reboot=shutdown,shutdown=pause"], stdin=subprocess.DEVNULL)
    results = []

    def result(name, ok, msg=""):
        results.append((name, ok))
        print(f"check {name}: {'OK' if ok else 'FAIL'}" + (f" ({msg})" if msg else ""))

    try:
        for _ in range(100):
            if os.path.exists(sock): break
            if q.poll() is not None: raise Infra("QEMU exited at start")
            time.sleep(0.1)
        qmp = wt.Qmp(sock)

        def lines():
            with open(serial, "rb") as fh:
                return [r.rstrip() for r in sm.render(fh.read())]

        def wait_for(pred, timeout):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if q.poll() is not None: raise Infra("QEMU exited")
                found = pred(lines())
                if found: return found
                time.sleep(0.2)
            return None

        def type_(s):
            for ch in s:
                keys = sm.KEYNAMES.get(ch, ch).split("-")
                qmp.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in keys], **{"hold-time": 40})
                time.sleep(0.12)

        def command(cmd, want, timeout=20):
            """Type `cmd`; wait until a line after its echo satisfies `want`
            (a regex). Returns the match or None."""
            start = len(lines())
            type_(cmd + "\n")
            def pred(ls):
                for r in ls[max(start - 1, 0):]:
                    m = re.search(want, r)
                    if m: return m
                return None
            return wait_for(pred, timeout)

        if not wait_for(lambda ls: any("l_os>" in r for r in ls), 180):
            raise Infra("no shell prompt on the serial console:\n" + "\n".join(lines()[-20:]))
        boot = "\n".join(lines())
        for want in ("tastatur", "usb: boot-stick an port"):
            if want not in boot:
                raise Infra(f"boot log lacks {want!r} (xHCI enumeration / stick):\n{boot[-1500:]}")
        time.sleep(1)

        # usb_keyboard_types_into_shell
        typed = command("echo usbkbd4711", r"^usbkbd4711$")
        sec = command("security", r"usb: tastatur (ja|nein), stick: (\w+), stick-lesezugriffe (\d+), tastatur-berichte (\d+)")
        if typed and sec:
            reports = int(sec.group(4))
            result("usb_keyboard_types_into_shell", sec.group(1) == "ja" and reports > 0,
                   f"echo output seen, tastatur {sec.group(1)}, berichte {reports}")
        else:
            result("usb_keyboard_types_into_shell", False,
                   f"echo output {'seen' if typed else 'missing'}, security line {'seen' if sec else 'missing'}; "
                   f"last lines: {lines()[-8:]}")

        # stick_read_count_constant_after_seal
        before = sec
        r1 = command("run hello", r"\[process exited with code 18\]")
        r2 = command("run hello_rs", r"hello from Rust in ring 3")
        after = command("security", r"usb: tastatur (ja|nein), stick: (\w+), stick-lesezugriffe (\d+), tastatur-berichte (\d+)")
        if before and after and r1 and r2:
            n1, n2 = int(before.group(3)), int(after.group(3))
            sealed = before.group(2) == "versiegelt" and after.group(2) == "versiegelt"
            result("stick_read_count_constant_after_seal", sealed and n1 == n2 and n1 > 0,
                   f"stick {before.group(2)}/{after.group(2)}, reads {n1} -> {n2}")
        else:
            result("stick_read_count_constant_after_seal", False,
                   f"before={bool(before)} run hello={bool(r1)} run hello_rs={bool(r2)} after={bool(after)}; "
                   f"last lines: {lines()[-8:]}")
        reports_before_unplug = int(after.group(4)) if after else None

        # keyboard_unplug_does_not_wipe. Left Ctrl is held down on the USB
        # keyboard while it is unplugged: the kernel must emit its release,
        # or the shared hotkey detector keeps Ctrl set and a later PS/2
        # Alt+Backspace wipes. (The shell decodes with HandleControl::Ignore,
        # so a stuck Ctrl does not change letters; the hotkey is what shows
        # it.) input-send-event without `device` goes to the active keyboard
        # (usb-kbd); no up event is ever sent.
        qmp.cmd("input-send-event", events=[{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": "ctrl"}}}])
        time.sleep(1)
        qmp.cmd("device_del", id="kbd0")
        deadline = time.monotonic() + 10
        while not any(n == "DEVICE_DELETED" and d.get("device") == "kbd0" for _, n, d in qmp.events):
            if time.monotonic() > deadline:
                raise Infra("no DEVICE_DELETED event for kbd0")
            qmp.wait_event("__none__", 0.5)  # only pumps events into qmp.events
        time.sleep(1)
        ps2 = command("echo ps2ok0815", r"^ps2ok0815$")
        # Alt+Backspace via PS/2: harmless unless Ctrl is stuck.
        qmp.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in ("alt", "backspace")], **{"hold-time": 60})
        stuck = qmp.wait_event("SHUTDOWN", 5) is not None
        if stuck:
            result("keyboard_unplug_does_not_wipe", False, "PS/2 Alt+Backspace wiped: Ctrl stuck from the unplugged USB keyboard")
        else:
            sec2 = command("security", r"usb: tastatur (ja|nein), stick: (\w+), stick-lesezugriffe (\d+), tastatur-berichte (\d+)")
            time.sleep(1)
            sec3 = command("security", r"usb: tastatur (ja|nein), stick: (\w+), stick-lesezugriffe (\d+), tastatur-berichte (\d+)")
            time.sleep(1)
            shutdowns = [n for _, n, _ in qmp.events if n in ("SHUTDOWN", "RESET")]
            panic = any("PANIC" in r or "panicked" in r for r in lines())
            # Typed through PS/2 now: no USB report arrives between the two
            # `security` outputs. (The count right before the unplug may still
            # grow by the release report of the last USB-typed Enter.)
            ok = (bool(ps2) and bool(sec2) and bool(sec3) and sec2.group(1) == "nein" and sec3.group(1) == "nein"
                  and sec2.group(4) == sec3.group(4) and not shutdowns and not panic)
            result("keyboard_unplug_does_not_wipe", ok,
                   f"PS/2 echo {'seen' if ps2 else 'missing'}, tastatur {sec2.group(1) if sec2 else '?'}, "
                   f"berichte {sec2.group(4) if sec2 else '?'} -> {sec3.group(4) if sec3 else '?'} "
                   f"(before unplug {reports_before_unplug}), SHUTDOWN/RESET events {shutdowns}, panic {panic}")
        if not stuck and q.poll() is not None:
            result("qemu_still_running", False, "QEMU exited")
    finally:
        if q.poll() is None:
            q.kill()
        q.wait()
    failed = [n for n, ok in results if not ok]
    if failed:
        print("---- console (last 40 lines) ----")
        with open(serial, "rb") as fh:
            print("\n".join(sm.render(fh.read())[-40:]))
    print("PASS" if not failed else "FAIL: " + ", ".join(failed))
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
