# l_os M2 "Amnesisches Live-System" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** l_os vergisst beim Abziehen des Boot-Sticks (QEMU `device_del`) oder per Notfall-Hotkey alles: Eingabe sperren, Bildschirm schwärzen, Schlüssel, Nutzerprozesse und alle Frames nullen, ACPI-Poweroff, ersatzweise Reset; ein Speicherabbild belegt das, die Wipe-Dauer ist gemessen; der Serial-Spiegel ist im normalen Image aus.

**Architecture:** Alle Entscheidungslogik (Wipe-Stufen, Bereichsarithmetik, Zeitmathematik, Hotkey, ACPI-Tabellen, xHCI-TRBs/Ringe/Kontexte/PORTSC, USB-Deskriptoren, HID-Boot-Reports, Bulk-Only/SCSI, Boot-Marker, PCI-BARs) lebt als reiner `#![forbid(unsafe_code)]`-Code in `crates/l_core` und wird auf dem Host getestet. Der Kernel liefert nur Hardwarezugriff: `src/acpi.rs`, `src/secrets.rs`, `src/wipe.rs` (Plan beim Boot, Executor ohne Heap und Locks, Schluss-`asm!`), `src/mmio.rs`, `src/xhci.rs`, `src/usb.rs`. Zuerst wird der Hotkey-Wipe vollständig geliefert und per Speicherabbild abgenommen (Tasks 1–11), danach kommt xHCI mit dem Abzieh-Auslöser (Tasks 12–24), zuletzt die Doku (Task 25).

**Tech Stack:** Rust `nightly-2026-10-11` (`rust-toolchain.toml`), `bootloader`/`bootloader_api` 0.11.17 (UEFI, OVMF), QEMU (`qemu-xhci`, `usb-storage`, `usb-kbd`, QMP, `dump-guest-memory`, `-action`), Host-Tests mit `cargo +stable` über `scripts/host_tests.sh`, Python 3 für die QEMU-Skripte.

**Spec:** `docs/superpowers/specs/2026-10-13-m2-amnesisch-design.md` (übergeordnet: `docs/superpowers/specs/2026-10-12-zukunftsplan-design.md` §5)

## Global Constraints

- Nichts darf als verifiziert beschrieben werden, was kein Test belegt (`docs/PLAN_NEXT.md`, Leitregeln); unbelegte Aussagen tragen **[assumed]**.
- Test zuerst schreiben, Fehlschlag sehen, dann implementieren; ein Commit pro Aufgabe; jede Aufgabe lässt den Baum grün.
- Vor jedem Commit: `scripts/host_tests.sh`, `cargo test` und `python3 -I scripts/qemu_shell_smoke.py` (sobald vom Task berührt), ab Task 11 zusätzlich `python3 -I scripts/qemu_wipe_test.py --trigger hotkey`, ab Task 22 `python3 -I scripts/qemu_usb_smoke.py`. Ohne QEMU/OVMF (z. B. in der Umgebung, in der dieser Plan entstand) laufen nur die Host-Tests; der Task gilt dann erst nach grünem CI-Lauf als abgeschlossen.
- Jeder neue `unsafe`-Block bekommt einen `// SAFETY:`-Kommentar; `crates/l_core` bleibt `#![forbid(unsafe_code)]`.
- Neue Abhängigkeit nur nach Prüfung von `no_std`, Allokation, `unsafe`, Wartung, Lizenz. M2 plant keine neue externe Crate (ACPI, xHCI, USB werden selbst und schmal implementiert; `tools/image` bekommt nur die Pfad-Abhängigkeit `l_core`).
- Toolchain bleibt `nightly-2026-10-11`.
- Daten nur im RAM: kein Codepfad darf auf einen Datenträger schreiben; Massenspeicher-Befehle nur aus der Whitelist `TEST UNIT READY`, `READ CAPACITY(10)`, `READ(10)` (nur Konstruktoren dafür in `l_core::usb_msc`).
- ISR-Regeln bleiben: keine Allokation im Interrupt-Kontext, Locks im ISR nur, wenn sie anderswo nur mit IF=0 gehalten werden.
- Wipe-Pfad: ab Stufe `InputLocked` keine Locks, kein Heap, keine Firmware-Tabellen; ab Stufe `KernelZeroed` nur Insel, Register, Code, `.rodata` und RELRO.
- `serial-console` ist nach Task 2 nie mehr default; kein Skript und keine Doku darf ein amnesisches Image mit Spiegel beschreiben.
- UI-Regeln aus M1 gelten weiter: höchstens drei Tastenhinweise, "amnesisch: ja" nur, wenn es stimmt (Spec E7).

## Review Focus

- PORTSC-Schreibzugriffe: nie `PED` mit 1 schreiben, nur gesetzte Change-Bits quittieren, `PP` erhalten (Tasks 13, 20, 23).
- Event-Ring: Cycle-Bit beim Umlauf, `ERDP` mit `EHB`, höchstens 16 Ereignisse pro Tick, kein Verlust bei vollem Ring (Tasks 12, 19, 23).
- Deskriptor-Parser: `bLength == 0`, `wTotalLength` größer als Puffer, abgeschnittene Deskriptoren: Fehler, nie Endlosschleife oder Panic (Task 14).
- Wipe-Executor: berührt er nach Stufe 6 den Heap, nach Stufe 9 ein `static` außerhalb der Insel, nimmt er einen Lock, liest er Firmware-Tabellen? (Task 10)
- Insel physisch nicht zusammenhängend; RELRO ausgenommen; Bereiche über 4 GiB; Überlauf der festen Listen ⇒ Plan ungültig, "amnesisch: nein" (Tasks 4, 9).
- Wipe aus einem ISR, während ein Nutzerprozess (fremdes CR3) lief (Task 10).
- DMA nach dem Nullen: Bus-Master aus und xHCI angehalten, bevor Stufe 6 beginnt (Task 10).
- Tastatur abziehen ⇒ kein Wipe; Stick abziehen ⇒ Wipe; Controller liest `0xFFFF_FFFF` ⇒ Wipe (Task 23).
- `BOOT_ID` nur per `read_volatile` lesen (Konstantenfaltung), Platzhalter genau einmal im Kernel (Tasks 16, 17, 21).
- Hotkey: Autorepeat, AltGr (`E0 38`), rechte Strg (`E0 1D`), Pause-Sequenz (`E1 …`) darf nicht auslösen (Tasks 7, 15).
- Warteschleifen gegen einen toten Controller immer mit Timeout (Tasks 19–21).
- Serial-Spiegel default aus; Spiegel-Build zeigt nie "amnesisch: ja" (Tasks 2, 3, 21).

---

## Dateistruktur

**Neu in `crates/l_core/src/` (rein, host-getestet):**
- `wipe.rs` – `Trigger`, `Stage` (Reihenfolge Spec §8.2), `WipeState`, `Amnesia` (Statuslabel und Grund)
- `memrange.rs` – `PhysRange`, `RangeList<N>` (fest, ohne Heap), Abzug, Seitenläufe, Bitmap-Iteration
- `zeroize.rs` – `zeroize`, `is_zero`, `Secret<N>`
- `wipetime.rs` – TSC-Kalibrierung, Zyklen → µs, Durchsatz, `StageTimes`, Berichtszeile ohne Heap
- `acpi.rs` – RSDP, SDT-Header, RSDT/XSDT-Suche, FADT, `_S5`
- `hotkey.rs` – `HotkeyDetector` (Set-1)
- `xhci_ring.rs` – `Trb`, TRB-Typen, `Event`, `ProducerRing`, `EventRing`, ERST-Eintrag
- `xhci_ctx.rs` – `Caps`, `PortSc`, `PortChange`, `Speed`, Slot-/Endpoint-/Input-Kontexte, DCI, Intervall
- `usb_desc.rs` – `SetupPacket`, Geräte- und Konfigurationsdeskriptor, `DeviceKind`
- `usb_hid.rs` – Boot-Report-Diff, HID-Usage → Set-1
- `usb_msc.rs` – CBW/CSW, SCSI-CDBs (Whitelist), `READ CAPACITY(10)`
- `bootmark.rs` – Platzhalter, Boot-ID, LBA-0-Marker
- `pci_bar.rs` – BAR-Dekodierung und -Größe

**Geändert in `crates/l_core/src/`:** `lib.rs` (Module), `elf.rs` (`load_segments`), `syscall.rs` (`SYSINFO_AMNESIA`).

**Kernel neu:** `src/acpi.rs`, `src/secrets.rs`, `src/wipe.rs`, `src/mmio.rs`, `src/xhci.rs`, `src/usb.rs`.
**Kernel geändert:** `src/lib.rs`, `src/main.rs`, `src/memory.rs` (`USER_FRAMES`), `src/usermode.rs` (Nutzerseiten markieren), `src/input.rs` (`feed`, zweiter Produzent), `src/interrupts.rs`, `src/time.rs` (TSC), `src/serial.rs` (lockfreies `raw_write`), `src/framebuffer.rs` (lockfreies Schwärzen), `src/pci.rs` (Prog-IF, 64-Bit-BAR, lockfreier Bus-Master-Aus), `src/aurora.rs`, `src/syscall.rs`, `user/src/bin/shell.rs`.
**Tests neu:** `tests/acpi.rs`, `tests/wipe_plan.rs`, `tests/usb_xhci.rs`, `tests/usb_storage.rs`, `tests/usb_keyboard.rs`, `tests/usb_wipe_hooks.rs` (nur mit Feature `test-hooks`).
**Werkzeuge:** `tools/image/src/lib.rs`, `tools/image/src/main.rs`, `tools/image/src/bin/l_os_run.rs`, `tools/image/Cargo.toml`/`Cargo.lock` (Pfad-Abhängigkeit `l_core`).
**Skripte/CI:** `scripts/qemu_wipe_test.py` (neu), `scripts/qemu_usb_smoke.py` (neu), `scripts/qemu_shell_smoke.py`, `scripts/qemu_fetch_smoke.py`, `.cargo/config.toml` (Alias), `Cargo.toml`, `.github/workflows/ci.yml`.
**Doku:** `docs/superpowers/notes/2026-10-13-usb-qemu-spike.md` (Task 1), am Ende `docs/ROADMAP.md`, `docs/THREAT_MODEL.md`, `docs/SECURITY.md`, `docs/ARCHITECTURE.md`, `README.md`, Zukunftsplan-Spec §5 (Messwert statt **[assumed]**).

---

## Teil A – Hotkey-Wipe (eigenständig abnahmefähig)

### Task 1: Spike – QEMU/OVMF-Fakten für USB, Abziehen und Speicherabbild

Zweck: alle QEMU-/OVMF-Annahmen der Spec prüfen, bevor Code darauf baut. Läuft auf einem Rechner oder CI-Job mit QEMU und OVMF (in der Plan-Umgebung fehlen beide).

**Files:**
- Create: `docs/superpowers/notes/2026-10-13-usb-qemu-spike.md`
- Throwaway: Wegwerf-Kernel/-Skripte außerhalb von `c-prod`

**Interfaces:**
- Produces: Antworten (mit Befehl und Ausgabe) auf: (1) QEMU-Version in CI (`qemu-system-x86_64 --version` auf `ubuntu-24.04`); (2) bootet OVMF das Image als `-device usb-storage,bus=xhci.0,drive=stick,bootindex=0` mit `readonly=on`? (3) PCI-IDs, BAR0-Typ und -Lage, `HCSPARAMS1/2`, `HCCPARAMS1` (CSZ, AC64), `MaxScratchpadBufs`, Portanzahl/-protokolle von `qemu-xhci`; (4) an welchen Root-Port und mit welcher Geschwindigkeit `usb-storage`/`usb-kbd` hängen; (5) erreicht `sendkey` bzw. QMP `send-key` bei vorhandenem `usb-kbd` die USB-Tastatur oder PS/2? (6) erzeugt QMP `device_del` eine Port-Status-Change mit `CCS=0`? (7) funktionieren `-action reboot=shutdown,shutdown=pause`, das QMP-Ereignis `SHUTDOWN` mit `reason` (`guest-shutdown`/`guest-reset`) und danach `dump-guest-memory` (ELF, PT_LOAD je RAM-Block) und `screendump`? (8) `_S5`-Wert im QEMU-DSDT (i440fx); (9) Speicherart (`Bootloader` vs `Usable`) der Frames von `.bss`, Boot-Kernelstack und Bootloader-Seitentabellen (Wegwerf-Kernel gibt `memory_regions` und übersetzte Adressen aus); (10) deckt das Physical-Memory-Mapping des Bootloaders den MMIO-Bereich unter 4 GiB ab (nur Info; Spec E5 nutzt ein eigenes Fenster).

- [ ] **Step 1:** QEMU-/OVMF-Version und Gerätehilfe festhalten: `qemu-system-x86_64 --version`, `qemu-system-x86_64 -device help | grep -E "xhci|usb-kbd|usb-storage"`.
- [ ] **Step 2:** Image mit `l_os_image` bauen, Boot von `usb-storage` versuchen (Fragen 2, 4); Bildschirmfoto per `screendump` in die Notiz.
- [ ] **Step 3:** Wegwerf-Kernel liest xHCI-Capability-Register und PORTSC aller Ports nach Boot und nach QMP `device_del` (Fragen 3, 6), gibt Speicherarten aus (Frage 9).
- [ ] **Step 4:** QMP-Ablauf `send-key` → `SHUTDOWN` → `dump-guest-memory` → `screendump` mit `-action` erproben (Fragen 5, 7); ACPI-Tabellen per `pmemsave` lesen und `_S5` notieren (Frage 8).
- [ ] **Step 5:** In der Notiz je Frage: Ergebnis, Befehl, Folge für Tasks 11, 19–24 (insbesondere: Rückfall "IDE-Boot + identisches Stick-Image" nach Spec §7.3 nötig oder nicht).
- [ ] **Step 6:** Commit `docs: USB/QEMU spike for M2`.

### Task 2: Serial-Spiegel standardmäßig aus

**Files:**
- Modify: `Cargo.toml` (`default = []`, Feature `wipe-test`), `.cargo/config.toml` (`[alias] run-serial = "run --features serial-console"`), `scripts/qemu_shell_smoke.py`, `scripts/qemu_fetch_smoke.py`, `src/fb_console.rs` (Doc-Kommentar), `src/lib.rs` (Konstante), `README.md`
- Test: `scripts/qemu_shell_smoke.py` (neue Prüfung)

**Interfaces:**
- Produces: `pub const SERIAL_MIRROR: bool = cfg!(feature = "serial-console");` in `src/lib.rs`; Cargo-Feature `wipe-test` (noch ohne Wirkung); Smoke-Funktion `default_build_has_no_serial_mirror()`.

- [ ] **Step 1: Failing test:** In `scripts/qemu_shell_smoke.py` Prüfung `default_build_has_no_serial_mirror`: `cargo build` ohne Features, Image bauen, booten, nach 5 s `echo nomirror4711` tippen, 2 s warten; das Serial-Log darf `nomirror4711` nicht enthalten. Der bestehende Ablauf baut künftig mit `cargo build --features serial-console`.
- [ ] **Step 2:** `python3 -I scripts/qemu_shell_smoke.py` → FAIL (`default_build_has_no_serial_mirror`: Text im Log, weil default an).
- [ ] **Step 3:** `default = []` setzen; `scripts/qemu_fetch_smoke.py` auf `cargo build --features serial-console`; Alias eintragen; README-Satz "build with `--no-default-features` to switch the mirror off" ersetzen durch "der Spiegel ist aus; `cargo run-serial` schaltet ihn für die Entwicklung ein, nie für ein amnesisches Image".
- [ ] **Step 4:** `cargo test` (ohne Features) → alle bisherigen Tests PASS (belegt Spec §5: Kernel-Tests hängen nicht am Spiegel; sollte einer doch daran hängen, bekommt er `required-features = ["serial-console"]` und wird in CI mit dem Feature aufgerufen – im Commit begründen); `python3 -I scripts/qemu_shell_smoke.py` → PASS; `scripts/net_tests.sh` → PASS.
- [ ] **Step 5:** Commit `build: serial-console mirror off by default; smoke builds enable it`.

### Task 3: Wipe-Zustandsmaschine und Statuslabel (`l_core::wipe`)

**Files:**
- Create: `crates/l_core/src/wipe.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum Trigger { StickRemoved, Hotkey, ControllerLost }` mit `as_str(&self) -> &'static str` (`"unplug"`, `"hotkey"`, `"controller"`)
  - `pub enum Stage { Armed, InputLocked, ScreenBlanked, DmaStopped, StackSwitched, KeysZeroed, UserZeroed, AllocatedZeroed, UsableZeroed, KernelZeroed, Reported, PowerOff, ResetFallback, Halted }` (`#[repr(u8)]`), `pub const ORDER: [Stage; 14]`, `Stage::next(self) -> Option<Stage>`, `Stage::from_u8(u8) -> Option<Stage>`
  - `pub struct WipeState` mit `new() -> Self` (Stage `Armed`, kein Auslöser), `trigger(&mut self, t: Trigger) -> bool` (nur beim ersten Mal `true`), `advance(&mut self, done: Stage) -> Result<(), OrderError>` (nur die jeweils nächste Stufe), `stage()`, `trigger_kind()`
  - `pub struct Amnesia { pub stick_sealed: bool, pub plan_ready: bool, pub serial_mirror: bool }` mit `armed(&self) -> bool`, `label(&self) -> &'static str` (`"amnesisch: ja"`/`"amnesisch: nein"`), `reason(&self) -> &'static str` (`""`, `"Serial-Spiegel an"`, `"kein Boot-Stick"`, `"Wipe-Plan fehlt"`; Spiegel hat Vorrang)

- [ ] **Step 1: Failing tests:** `stage_order_matches_spec_section_5` (Schlüssel vor Nutzer vor allen Frames vor Poweroff vor Reset; `InputLocked` und `ScreenBlanked` vor jedem Nullen), `stage_next_walks_order_and_ends_at_halted`, `stage_u8_roundtrip_and_unknown_is_none`, `first_trigger_wins`, `advance_rejects_skipping_and_going_back`, `advance_before_trigger_is_error`, `amnesia_label_ja_only_when_all_true`, `serial_mirror_forces_nein_and_is_reported_first`.
- [ ] **Step 2:** `scripts/host_tests.sh wipe::` → FAIL (Modul fehlt).
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh wipe::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): wipe stages and amnesia status`.

### Task 4: Bereichsarithmetik und Zeroize-Helfer (`memrange`, `zeroize`)

**Files:**
- Create: `crates/l_core/src/memrange.rs`, `crates/l_core/src/zeroize.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces (`memrange`, alles ohne Heap):
  - `pub struct PhysRange { pub start: u64, pub end: u64 }` (halboffen) mit `len()`, `is_empty()`, `page_aligned(&self) -> bool`
  - `pub struct RangeList<const N: usize>` mit `new()`, `push(&mut self, r: PhysRange) -> Result<(), Full>` (fasst angrenzende/überlappende zusammen, sortiert), `iter()`, `len()`, `total_bytes()`, `subtract(&self, excl: &[PhysRange]) -> Result<RangeList<N>, Full>`, `contains(&self, addr: u64) -> bool`
  - `pub fn coalesce_pages<const N: usize>(pages: impl Iterator<Item = u64>) -> Result<RangeList<N>, Full>` (4-KiB-Seitenadressen → Läufe)
  - `pub fn for_each_set_bit(words: usize, word: impl Fn(usize) -> u64, f: impl FnMut(u64))` (Frame-Nummern)
  - `pub fn unallocated_runs(region: PhysRange, tracked_frames: u64, is_allocated: impl Fn(u64) -> bool, f: impl FnMut(PhysRange))` (Frames ab `tracked_frames` gelten immer als nicht vergeben; Läufe maximal zusammengefasst)
- Produces (`zeroize`): `pub fn zeroize(buf: &mut [u8])` (Schreiben + `compiler_fence(SeqCst)` + `core::hint::black_box`), `pub fn is_zero(buf: &[u8]) -> bool`, `pub struct Secret<const N: usize>` mit `new()`, `expose(&self) -> &[u8; N]`, `expose_mut()`, `wipe(&mut self)`, `Drop` ruft `wipe`, `Debug` zeigt nur `Secret<N>([redacted])`, kein `Clone`/`Copy`.

- [ ] **Step 1: Failing tests:** `push_merges_adjacent_and_overlapping`, `push_keeps_sorted`, `push_full_is_error_not_panic`, `subtract_splits_range`, `subtract_non_contiguous_island`, `subtract_exclusion_outside_is_noop`, `total_bytes_after_subtract`, `coalesce_pages_runs`, `for_each_set_bit_reports_frame_numbers`, `unallocated_runs_skips_allocated_and_merges`, `unallocated_runs_above_tracked_limit_are_included`, `zeroize_clears_all_bytes`, `is_zero_detects_single_byte`, `secret_wipe_clears`, `secret_debug_does_not_print_contents`.
- [ ] **Step 2:** `scripts/host_tests.sh memrange::` und `zeroize::` → FAIL.
- [ ] **Step 3:** Implementieren (feste Arrays, `checked_add` bei allen Grenzen).
- [ ] **Step 4:** `scripts/host_tests.sh memrange::` und `zeroize::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): physical range lists and zeroize helpers`.

### Task 5: Zeitmessungs-Mathematik (`wipetime`)

**Files:**
- Create: `crates/l_core/src/wipetime.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Consumes: `Stage`, `Trigger` (Task 3).
- Produces: `pub fn tsc_hz(delta_cycles: u64, delta_ticks: u64, tick_hz: u32) -> Option<u64>` (`None` bei 0 Ticks), `pub fn cycles_to_us(cycles: u64, hz: u64) -> u64` (u128, sättigend, `hz == 0` → 0), `pub fn mib_per_s(bytes: u64, us: u64) -> u64`, `pub struct StageTimes { start: u64, at: [u64; 14] }` mit `start(cycles)`, `mark(stage, cycles)`, `us_since_start(stage, hz) -> Option<u64>`, `pub fn report_line(times: &StageTimes, trigger: Trigger, hz: u64, bytes: u64, out: &mut [u8]) -> usize` (Format `WIPE-REPORT trigger=<t> keys_us=<n> user_us=<n> allocated_us=<n> usable_us=<n> kernel_us=<n> total_us=<n> bytes=<n>\n`, schneidet bei zu kleinem Puffer ab, nie Panic; schreibt über `core::fmt::Write` in einen festen Puffer).

- [ ] **Step 1: Failing tests:** `tsc_hz_from_100hz_ticks` (2_000_000_000 Zyklen in 100 Ticks bei 100 Hz → 2 GHz), `tsc_hz_zero_ticks_is_none`, `cycles_to_us_no_overflow_at_u64_max`, `mib_per_s_rounding`, `stage_times_relative_to_start`, `unmarked_stage_is_none`, `report_line_format_is_stable` (exakter String; dieses Format parst Task 11), `report_line_truncates_without_panic`.
- [ ] **Step 2:** `scripts/host_tests.sh wipetime::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh wipetime::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): wipe timing math and report line`.

### Task 6: ACPI-Tabellen (`l_core::acpi`)

**Files:**
- Create: `crates/l_core/src/acpi.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum AcpiError { BadSignature, BadChecksum, Truncated, NotFound, Unsupported }`
  - `pub struct Rsdp { pub revision: u8, pub rsdt: u32, pub xsdt: Option<u64> }`, `pub fn parse_rsdp(b: &[u8]) -> Result<Rsdp, AcpiError>` (Signatur `"RSD PTR "`, Prüfsumme über 20 Byte; ab Revision 2 erweiterte Prüfsumme über `length`)
  - `pub struct SdtHeader { pub signature: [u8; 4], pub length: u32 }`, `pub fn parse_sdt(b: &[u8]) -> Result<SdtHeader, AcpiError>` (Prüfsumme über `length`)
  - `pub fn find_table(rsdp: &Rsdp, sig: [u8; 4], read: impl FnMut(u64, &mut [u8]) -> bool) -> Result<u64, AcpiError>` (XSDT vor RSDT; `read` liest physischen Speicher; Eintragsanzahl aus `length`, Obergrenze 64)
  - `pub struct Fadt { pub pm1a_cnt: u16, pub pm1b_cnt: u16, pub dsdt: u64, pub smi_cmd: u32, pub acpi_enable: u8 }`, `pub fn parse_fadt(b: &[u8]) -> Result<Fadt, AcpiError>` (Legacy-Blöcke; bei 0 `X_PM1a/b_CNT_BLK`, nur Adressraum System-I/O, sonst `Unsupported`; `X_DSDT` vor `DSDT`)
  - `pub fn find_s5(dsdt: &[u8]) -> Option<(u8, u8)>` (`_S5_` mit optionalem `\`, `PackageOp` 0x12, PkgLength 1–4 Byte, Elemente als `0x0A nn`, `0x00`, `0x01`)
  - `pub fn s5_value(slp_typ: u8) -> u16` (`(slp_typ & 7) << 10 | 1 << 13`)

- [ ] **Step 1: Failing tests** (Tabellen im Test synthetisch gebaut, Prüfsummen berechnet): `rsdp_v1_ok`, `rsdp_bad_checksum_rejected`, `rsdp_v2_extended_checksum_checked`, `xsdt_preferred_over_rsdt`, `find_table_not_found`, `sdt_length_beyond_read_is_truncated_error`, `fadt_legacy_pm1a`, `fadt_x_block_used_when_legacy_zero`, `fadt_x_block_memory_space_unsupported`, `fadt_x_dsdt_preferred`, `s5_byteprefix_values`, `s5_zero_and_one_ops`, `s5_with_root_prefix`, `s5_missing_is_none`, `s5_truncated_does_not_panic`, `s5_value_sets_slp_en`.
- [ ] **Step 2:** `scripts/host_tests.sh acpi::` → FAIL.
- [ ] **Step 3:** Implementieren (alle Zugriffe über `get`, keine Indizes ohne Prüfung).
- [ ] **Step 4:** `scripts/host_tests.sh acpi::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): ACPI table parsing for S5 poweroff`.

### Task 7: Notfall-Hotkey-Erkennung (`l_core::hotkey`)

**Files:**
- Create: `crates/l_core/src/hotkey.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces: `pub struct HotkeyDetector` mit `new()`, `feed(&mut self, scancode: u8) -> bool` (`true` beim Make-Code der Rücktaste `0x0E`, solange eine Strg (`1D`, `E0 1D`) und eine Alt (`38`, `E0 38`) gedrückt sind), `to_bits(&self) -> u8`, `from_bits(u8) -> Self` (Zustand passt in ein `AtomicU8`, damit ISRs ihn ohne Lock halten).

- [ ] **Step 1: Failing tests:** `ctrl_alt_backspace_fires`, `right_ctrl_and_altgr_fire`, `backspace_alone_does_not_fire`, `ctrl_backspace_without_alt_does_not_fire`, `release_clears_modifiers` (Break-Codes `9D`, `B8`, `E0 9D`, `E0 B8`), `e0_prefix_then_other_key_is_harmless`, `pause_sequence_does_not_fire` (`E1 1D 45 E1 9D C5` bei gehaltenem Alt), `autorepeat_fires_again` (wiederholter Make-Code; der Wipe ist idempotent), `bits_roundtrip_preserves_state`.
- [ ] **Step 2:** `scripts/host_tests.sh hotkey::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh hotkey::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): emergency hotkey detector (Ctrl+Alt+Backspace)`.

### Task 8: ACPI im Kernel (`src/acpi.rs`)

**Files:**
- Create: `src/acpi.rs`, `tests/acpi.rs`
- Modify: `src/lib.rs` (Modul, `init_memory` merkt `rsdp_addr`), `Cargo.toml` (`[[test]] name = "acpi"`)

**Interfaces:**
- Consumes: `l_core::acpi` (Task 6), `memory::phys_offset()`.
- Produces: `pub struct PowerOff { pub pm1a_cnt: u16, pub pm1b_cnt: u16, pub value_a: u16, pub value_b: u16 }`, `pub fn init(rsdp_addr: Option<u64>)` (einmal beim Boot; liest Tabellen über das Offset-Mapping, schaltet ACPI bei Bedarf per `SMI_CMD` ein, begrenztes Warten auf `SCI_EN`), `pub fn power_off_plan() -> Option<PowerOff>`.

- [ ] **Step 1: Failing test** `tests/acpi.rs`: `finds_s5_in_qemu_tables` (`power_off_plan()` ist `Some`, `pm1a_cnt != 0`, `value_a & (1 << 13) != 0`; Werte über Serial ausgeben), `sci_enabled_after_init` (Bit 0 von `PM1a_CNT` gesetzt).
- [ ] **Step 2:** `cargo test --test acpi` → FAIL (Modul fehlt).
- [ ] **Step 3:** Implementieren; physische Lesezugriffe über `phys_offset() + addr` mit Längenprüfung gegen `SdtHeader::length`.
- [ ] **Step 4:** `cargo test --test acpi` → PASS; `cargo test` → PASS.
- [ ] **Step 5:** Commit `feat(acpi): find PM1 control block and S5 sleep type`.

### Task 9: Wipe-Plan beim Boot (Schlüssel-Slots, Nutzer-Bitmap, Kernel-Segmente)

**Files:**
- Create: `src/secrets.rs`, `src/wipe.rs` (nur Plan/Arm-Teil), `tests/wipe_plan.rs`
- Modify: `crates/l_core/src/elf.rs` (`load_segments`), `src/memory.rs` (`USER_FRAMES`, `mark_user_frame`, Löschen in `destroy_address_space`, `pub fn allocated_word(i) -> u64` lockfrei lesbar), `src/usermode.rs` (Nutzer-Leaf-Frames markieren, Zeile mit `map_to_with_table_flags`), `src/lib.rs` (`BootFacts` aus `BootInfo` sichern: `rsdp_addr`, `kernel_addr`, `kernel_len`, `kernel_image_offset`, `kernel_stack_bottom`, `kernel_stack_len`, Speicherkarte), `src/framebuffer.rs` (Basis/Länge in Atomics), `Cargo.toml` (`[[test]] name = "wipe_plan"`)

**Interfaces:**
- Consumes: `memrange` (Task 4), `acpi::power_off_plan` (Task 8), `l_core::wipe::Amnesia` (Task 3).
- Produces:
  - `l_core::elf::load_segments(bytes: &[u8]) -> Result<LoadInfo, ElfError>` mit `pub struct LoadInfo { pub segments: [Option<Segment>; MAX_SEGMENTS], pub relro: Option<(u64, u64)> }` (nimmt `ET_EXEC` und `ET_DYN`, prüft kein Fenster)
  - `secrets::SLOT_SIZE = 4096`, `secrets::SLOTS = 4`, `secrets::slot_ranges() -> [(VirtAddr, usize); SLOTS]`, `secrets::with_slot(i, f)`
  - `memory::USER_FRAMES` (AtomicU64-Bitmap, 4 GiB), `memory::is_user_frame(n) -> bool`
  - `wipe::arm(facts: &BootFacts) -> Result<(), ArmError>` füllt `ISLAND.plan`: `usable: RangeList<64>`, `kernel_rw: RangeList<256>` (PF_W-Segmente ohne RELRO und Boot-Kernelstack, minus Insel), `island_phys: RangeList<8>`, `keys: [(u64 virt, usize); SLOTS]`, `fb: (u64, usize)`, `power: Option<PowerOff>`, `tsc_hz`; `wipe::amnesia() -> Amnesia` (Feld `plan_ready`)
  - `#[repr(C, align(4096))] static mut ISLAND: Island` mit `stack: [u8; 16384]`, `plan: Plan`, `times: StageTimes` (Zugriff nur über `wipe.rs`)

- [ ] **Step 1: Failing tests:** Host: `load_segments_accepts_et_dyn`, `load_segments_reports_relro` (in `elf.rs`, mit synthetischem ELF). Kernel `tests/wipe_plan.rs`: `plan_covers_every_usable_byte` (Summe `usable` = `memory::usable_bytes`), `island_and_code_not_in_usable` (Insel-Seiten und die physische Seite von `kernel_main` liegen in keiner `Usable`-Region; belegt Spec §8.3a), `kernel_rw_excludes_island_and_relro`, `kernel_rw_contains_a_bss_static` (physische Adresse eines Test-`static` liegt in `kernel_rw`), `user_frames_marked_and_cleared` (Prozess starten: Bits gesetzt; nach Ende: alle 0), `amnesia_plan_ready_after_arm`.
- [ ] **Step 2:** `scripts/host_tests.sh elf::` und `cargo test --test wipe_plan` → FAIL.
- [ ] **Step 3:** Implementieren; `arm` wird in `init_memory` nach `acpi::init` aufgerufen; Überlauf einer Liste ⇒ `ArmError`, `plan_ready = false`, Meldung auf der Konsole.
- [ ] **Step 4:** `scripts/host_tests.sh`, `cargo test` → PASS.
- [ ] **Step 5:** Commit `feat(wipe): precomputed wipe plan, key slots and user-frame bitmap`.

### Task 10: Wipe-Executor, Hotkey im Tastatur-ISR, Feature `wipe-test`

**Files:**
- Modify: `src/wipe.rs` (Executor), `src/input.rs` (`pub fn feed(byte: u8)`: Push + `HotkeyDetector` in `AtomicU8`, bei Treffer `wipe::trigger(Trigger::Hotkey)`; Doc: zwei Produzenten im ISR-Kontext), `src/interrupts.rs` (Tastatur-ISR ruft `input::feed`, verwirft Eingaben bei `WIPING`), `src/time.rs` (`calibrate_tsc()` über 5 PIT-Ticks beim Boot, `tsc_hz()`), `src/serial.rs` (`raw_write(bytes)`: UART 0x3F8 polling ohne Lock und ohne Statics), `src/framebuffer.rs` (`blank_raw()`), `src/pci.rs` (`disable_bus_master_unlocked(&Device)`), `src/lib.rs`, `Cargo.toml` (Feature `test-hooks = []`)
- Test: in-crate `#[test_case]` in `src/wipe.rs` für die nicht-terminalen Teile

**Interfaces:**
- Consumes: Tasks 3, 4, 5, 7, 8, 9.
- Produces: `pub fn trigger(t: Trigger) -> !` bzw. kehrt sofort zurück, wenn schon ein Wipe läuft (dann `hlt`-Schleife mit IF=0); Stufen exakt nach `Stage::ORDER`; Schluss-`asm!` (`options(noreturn)`): Insel per `rep stosq` über ihre virtuelle Adresse, `wbinvd`, `out` PM1a/PM1b, Warteschleife, `0xCF9` ← 0x06, `0x64` ← 0xFE, Null-IDT + `int3`, `cli; hlt`. Stufe `DmaStopped` löscht in diesem Task nur die Bus-Master-Bits; den xHCI-Halt ergänzt Task 19. Feature `test-hooks` (nie default, nie zusammen mit `wipe-test`): `trigger` zählt nur (`wipe::test_hook_count()`), und `amnesia()` meldet `plan_ready = false`, damit ein solcher Build nie "amnesisch: ja" zeigt. Mit `wipe-test`: beim Boot `wipe-test: key-slot phys=0x.. len=4096`, je `Usable`-Bereich `wipe-test: usable 0x..-0x..`, `wipe-test: armed`; Slot 0 aus RDRAND gefüllt; vor `PowerOff` die `WIPE-REPORT`-Zeile über `raw_write`.

- [ ] **Step 1: Failing tests** (in-crate, laufen mit `cargo test --lib`): `feed_pushes_and_detects_hotkey_without_wiping_in_test` (Detektor-Treffer wird unter `cfg(test)` bzw. mit Feature `test-hooks` über einen Hook gezählt statt ausgeführt), `blank_raw_zeroes_framebuffer` (danach alle Bytes 0, über `with_framebuffer`), `disable_bus_master_clears_bit2` (virtio-net: Bit 2 im PCI-Command danach 0, anschließend wieder setzen), `raw_write_needs_no_lock` (mit gehaltenem `SERIAL1`-Lock aufrufbar), `stage_sequence_of_dry_run` (Executor im Trockenlauf-Modus: ruft Stufenfunktionen in Reihenfolge auf, nullt nur einen Testpuffer, `WipeState` endet bei `Reported`).
- [ ] **Step 2:** `cargo test --lib` → FAIL.
- [ ] **Step 3:** Implementieren. Stufe 4 wechselt CR3 und Stack in einem kleinen `asm!`, der danach eine `extern "C" fn stage_rest() -> !` anspringt. Alle Schleifen über feste Listen der Insel; kein `alloc`, keine `Mutex`. Jeder `unsafe`-Block mit `// SAFETY:` (warum die Adresse gültig ist, warum niemand mehr darauf zugreift).
- [ ] **Step 4:** `cargo test`, `python3 -I scripts/qemu_shell_smoke.py` → PASS (der Hotkey wird dort nie gedrückt).
- [ ] **Step 5:** Commit `feat(wipe): executor, emergency hotkey, wipe-test reporting`.

### Task 11: Abnahme Hotkey-Wipe per Speicherabbild (`scripts/qemu_wipe_test.py`)

**Files:**
- Create: `scripts/qemu_wipe_test.py`
- Modify: `.github/workflows/ci.yml` (Schritt "Wipe test (hotkey)")

**Interfaces:**
- Consumes: Tasks 2, 10; Hilfsfunktionen aus `scripts/qemu_shell_smoke.py` (`tool`, `parse_ppm`) werden per `importlib.util.spec_from_file_location` über den expliziten Pfad geladen (kein `sys.path`-Eingriff, `-I` bleibt wirksam).
- Produces: `python3 -I scripts/qemu_wipe_test.py --trigger hotkey|unplug [--mem 256M] [--boot ide|usb] [--keep-dump]`; QEMU mit `-m`, `-cpu max`, VGA 1024x768, `-display none`, `-serial file:`, `-qmp unix:…`, `-action reboot=shutdown,shutdown=pause`; Build `cargo build --features wipe-test`.

- [ ] **Step 1: Failing checks** (Skript zuerst, gegen den Stand ohne Task 10 einmal laufen lassen bzw. mit absichtlich ausgelassener Stufe 6 als Mutationsprobe): `wipe_report_present` (genau eine `WIPE-REPORT`-Zeile, `trigger=hotkey`), `poweroff_not_reset` (QMP `SHUTDOWN` mit `reason == "guest-shutdown"`), `key_slot_zero_after_wipe` (vor dem Auslösen per `pmemsave` gelesen: nicht null; danach im Abbild: null), `usable_ram_zero_after_wipe` (jedes Byte aller `wipe-test: usable`-Bereiche null), `sentinel_absent_from_dump` (zufälliger 24-Zeichen-Text, per `echo` getippt, und die ersten 32 Byte des Schlüssel-Slots kommen im ganzen Abbild nicht vor), `screen_black_after_wipe` (`screendump` nur Nullpixel), `serial_log_has_no_sentinel`.
- [ ] **Step 2:** Mutationsprobe: Stufe `UserZeroed` testweise auslassen → `sentinel_absent_from_dump` oder `usable_ram_zero_after_wipe` FAIL; Stufe `ScreenBlanked` auslassen → `screen_black_after_wipe` FAIL. Ergebnis im Commit-Text festhalten, Mutation zurücknehmen.
- [ ] **Step 3:** `python3 -I scripts/qemu_wipe_test.py --trigger hotkey` → PASS; Zeiten aus `WIPE-REPORT` und Host-Zeit (QMP-Befehl → `SHUTDOWN`) ausgeben.
- [ ] **Step 4:** CI-Schritt ergänzen; `cargo test`, Smoke → PASS.
- [ ] **Step 5:** Commit `test(wipe): memory-dump acceptance for the hotkey wipe`.

---

## Teil B – xHCI, Boot-Stick und Abzieh-Auslöser

### Task 12: xHCI-TRBs und Ringe (`l_core::xhci_ring`)

**Files:**
- Create: `crates/l_core/src/xhci_ring.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Trb { pub param: u64, pub status: u32, pub control: u32 }` mit `to_words() -> [u32; 4]`, `from_words([u32; 4])`, `trb_type() -> u8`, `cycle() -> bool`, `with_cycle(bool) -> Trb`
  - TRB-Typ-Konstanten (`NORMAL = 1`, `SETUP = 2`, `DATA = 3`, `STATUS = 4`, `LINK = 6`, `ENABLE_SLOT = 9`, `ADDRESS_DEVICE = 11`, `CONFIGURE_ENDPOINT = 12`, `EVALUATE_CONTEXT = 13`, `NOOP_CMD = 23`, `TRANSFER_EVENT = 32`, `COMMAND_COMPLETION = 33`, `PORT_STATUS_CHANGE = 34`, `HOST_CONTROLLER = 37`) und Completion-Codes (`SUCCESS = 1`, `STALL = 6`, `SHORT_PACKET = 13`, …)
  - Konstruktoren: `Trb::noop_cmd()`, `enable_slot()`, `address_device(input_ctx: u64, slot: u8, bsr: bool)`, `configure_endpoint(input_ctx: u64, slot: u8)`, `evaluate_context(input_ctx: u64, slot: u8)`, `setup(packet: u64, trt: u8)`, `data(buf: u64, len: u32, dir_in: bool)`, `status(dir_in: bool)`, `normal(buf: u64, len: u32, ioc: bool)`, `link(target: u64, toggle_cycle: bool)`
  - `pub enum Event { CommandCompletion { trb: u64, code: u8, slot: u8 }, Transfer { trb: u64, code: u8, residual: u32, slot: u8, endpoint: u8 }, PortStatusChange { port: u8 }, HostController { code: u8 }, Other(u8) }`, `Event::decode(Trb) -> Event`
  - `pub struct ProducerRing { base: u64, len: usize, enqueue: usize, cycle: bool }` mit `new(base, len)` (`len ≥ 2`, letzter Platz = Link), `enqueue(&mut self, t: Trb) -> Enqueued` (`Enqueued { index, trb, link: Option<(usize, Trb)> }`; Aufrufer schreibt beides), `enqueue_ptr(&self) -> u64`
  - `pub struct EventRing { len: usize, dequeue: usize, cycle: bool }` mit `new(len)`, `poll(&mut self, read: impl Fn(usize) -> [u32; 4]) -> Option<Trb>`, `erdp(&self, base: u64) -> u64` (Dequeue-Adresse | `EHB` Bit 3)
  - `pub fn erst_entry(base: u64, size: u16) -> [u32; 4]`

- [ ] **Step 1: Failing tests:** `trb_words_roundtrip`, `noop_cmd_type_23`, `setup_trb_has_idt_and_len_8`, `cycle_bit_set_on_first_pass`, `link_trb_points_to_base_with_toggle`, `cycle_flips_after_wrap`, `ring_len_2_works`, `event_ring_empty_when_cycle_mismatch`, `event_ring_consumes_in_order_and_wraps`, `erdp_sets_ehb_and_points_at_dequeue`, `decode_port_status_change_port_id`, `decode_transfer_event_fields`, `decode_unknown_type_is_other`.
- [ ] **Step 2:** `scripts/host_tests.sh xhci_ring::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh xhci_ring::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): xHCI TRBs, producer and event rings`.

### Task 13: xHCI-Kontexte, Capabilities, PORTSC und Port-Ereignisse (`l_core::xhci_ctx`)

**Files:**
- Create: `crates/l_core/src/xhci_ctx.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Caps { pub cap_length: u8, pub max_slots: u8, pub max_ports: u8, pub max_scratchpad: u16, pub csz64: bool, pub ac64: bool, pub doorbell_offset: u32, pub runtime_offset: u32 }`, `Caps::decode(caplength_hciversion: u32, hcsparams1: u32, hcsparams2: u32, hccparams1: u32, dboff: u32, rtsoff: u32) -> Caps` (Scratchpad Hi 25:21 / Lo 31:27)
  - `pub struct PortSc(pub u32)` mit `ccs() ped() pr() pls() pp() speed() csc() pec() wrc() occ() prc() plc() cec()`, `is_all_ones()`, `ack_changes(self) -> u32` (Erhaltungsbits `PP` und Wake-Bits + gesetzte Change-Bits, `PED` immer 0), `reset_request(self) -> u32` (Erhaltungsbits + `PR`)
  - `pub enum PortChange { Connected(Speed), Disconnected, ResetDone, ControllerGone, Nothing }`, `pub fn classify(was_connected: bool, now: PortSc) -> PortChange`
  - `pub enum Speed { Full = 1, Low = 2, High = 3, Super = 4 }`, `Speed::from_psi(u8) -> Option<Speed>`, `Speed::ep0_max_packet(self) -> u16` (8, 8, 64, 512)
  - `pub fn dci(endpoint_address: u8) -> u8` (EP0 → 1; sonst `num * 2 + in`)
  - `pub fn ep_interval(speed: Speed, b_interval: u8) -> u8` (FS/LS-Interrupt: `floor(log2(b_interval * 8))`, begrenzt 3..=10; HS/SS: `b_interval - 1`, begrenzt 0..=15)
  - `pub struct SlotCtx { pub route: u32, pub speed: Speed, pub context_entries: u8, pub root_port: u8 }` → `encode() -> [u32; 8]`; `pub struct EndpointCtx { pub ep_type: u8, pub max_packet: u16, pub interval: u8, pub cerr: u8, pub dequeue: u64, pub dcs: bool, pub avg_trb_len: u16 }` → `encode() -> [u32; 8]`; `pub fn input_control(add: u32, drop: u32) -> [u32; 8]`; `pub fn ctx_offset(index: usize, csz64: bool) -> usize`

- [ ] **Step 1: Failing tests:** `caps_decode_qemu_like_values`, `scratchpad_hi_lo_combined`, `ack_never_writes_ped`, `ack_writes_only_set_change_bits`, `ack_preserves_power`, `reset_request_keeps_power_and_sets_pr`, `csc_with_ccs0_is_disconnect`, `ccs_drop_without_csc_is_disconnect`, `csc_with_ccs1_is_connect_with_speed`, `prc_is_reset_done`, `all_ones_is_controller_gone`, `dci_for_ep0_and_in_out`, `interval_fs_10ms_is_6`, `interval_hs_binterval_4_is_3`, `slot_ctx_fields_in_right_bits`, `ep_ctx_dequeue_carries_dcs`, `csz64_doubles_offsets`.
- [ ] **Step 2:** `scripts/host_tests.sh xhci_ctx::` → FAIL.
- [ ] **Step 3:** Implementieren (Bitlagen nach xHCI-Spezifikation 1.2; Quelle je Feld als Kommentar mit Abschnittsnummer).
- [ ] **Step 4:** `scripts/host_tests.sh xhci_ctx::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): xHCI contexts, capabilities and port status decoding`.

### Task 14: USB-Deskriptoren und Setup-Pakete (`l_core::usb_desc`)

**Files:**
- Create: `crates/l_core/src/usb_desc.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct SetupPacket { pub request_type: u8, pub request: u8, pub value: u16, pub index: u16, pub length: u16 }` mit `to_u64()`, `dir_in()`; Konstruktoren `get_descriptor(dtype: u8, index: u8, len: u16)`, `set_configuration(value: u8)`, `set_protocol_boot(iface: u8)`, `set_idle(iface: u8)`
  - `pub enum DescError { Truncated, BadLength, BadType, TooLong }`
  - `pub struct DeviceDescriptor { pub usb: u16, pub class: u8, pub max_packet0: u8, pub vendor: u16, pub product: u16, pub num_configs: u8 }`, `DeviceDescriptor::parse(&[u8]) -> Result<_, DescError>` (8-Byte-Präfix reicht für `max_packet0`, über `parse_prefix`)
  - `pub struct Endpoint { pub address: u8, pub attributes: u8, pub max_packet: u16, pub interval: u8 }`, `pub struct Interface { pub number: u8, pub class: u8, pub subclass: u8, pub protocol: u8, pub endpoints: [Option<Endpoint>; 4] }`, `pub struct Config { pub value: u8, pub total_length: u16, pub interfaces: [Option<Interface>; 4] }`, `pub fn parse_config(&[u8]) -> Result<Config, DescError>`
  - `pub enum DeviceKind { BootKeyboard { iface: u8, ep_in: Endpoint }, BulkOnlyStorage { iface: u8, ep_in: Endpoint, ep_out: Endpoint }, Unsupported }`, `pub fn classify(&Config) -> DeviceKind`

- [ ] **Step 1: Failing tests** (Byte-Folgen nach USB 2.0 Kap. 9 im Test): `setup_get_descriptor_device_bytes`, `set_protocol_boot_is_class_interface_request`, `device_descriptor_parse`, `device_prefix_8_bytes_gives_max_packet0`, `config_keyboard_classified`, `config_mass_storage_bbb_classified`, `config_storage_without_bulk_out_unsupported`, `blength_zero_is_error_not_loop`, `total_length_beyond_buffer_is_truncated`, `unknown_descriptors_are_skipped` (HID-Deskriptor 0x21 zwischen Interface und Endpoint), `more_than_four_interfaces_is_too_long`.
- [ ] **Step 2:** `scripts/host_tests.sh usb_desc::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh usb_desc::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): USB descriptor parsing and device classification`.

### Task 15: HID-Boot-Reports zu Set-1-Scancodes (`l_core::usb_hid`)

**Files:**
- Create: `crates/l_core/src/usb_hid.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Consumes: `HotkeyDetector` (Task 7) nur im Test.
- Produces: `pub struct BootReport(pub [u8; 8])`, `pub fn usage_to_set1(usage: u8) -> Option<(bool, u8)>` (`(e0_prefix, make)`), `pub fn modifier_to_set1(bit: u8) -> (bool, u8)`, `pub fn report_to_set1(prev: &BootReport, now: &BootReport, push: impl FnMut(u8))` (erst losgelassene, dann gedrückte Tasten; Modifier zuerst gedrückt, zuletzt losgelassen; Break = Make | 0x80, mit `E0`-Präfix wo nötig; Rollover-Fehlerbericht `01 01 01 …` wird ignoriert).

- [ ] **Step 1: Failing tests:** `a_press_release_gives_1e_9e`, `left_ctrl_gives_1d`, `right_ctrl_gives_e0_1d`, `arrow_up_gives_e0_48`, `enter_and_backspace`, `german_positions_by_usage` (Usage 0x2F → 0x1A, 0x33 → 0x27, 0x64 → 0x56), `rollover_error_report_is_ignored`, `unknown_usage_dropped`, `ctrl_alt_backspace_via_hid_fires_hotkey_detector`.
- [ ] **Step 2:** `scripts/host_tests.sh usb_hid::` → FAIL.
- [ ] **Step 3:** Implementieren (Tabelle Usage → Set-1 nach "HID Usage Tables" Kap. 10 und PS/2-Set-1; Quelle als Kommentar).
- [ ] **Step 4:** `scripts/host_tests.sh usb_hid::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): HID boot keyboard reports to set-1 scancodes`.

### Task 16: Bulk-Only/SCSI und Boot-Marker (`l_core::usb_msc`, `l_core::bootmark`)

**Files:**
- Create: `crates/l_core/src/usb_msc.rs`, `crates/l_core/src/bootmark.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Produces (`usb_msc`): `pub struct Cbw` nur über `Cbw::test_unit_ready(tag)`, `Cbw::read_capacity10(tag)`, `Cbw::read10(tag, lba: u32, blocks: u16, block_len: u32)` (kein öffentlicher Konstruktor für andere Befehle, kein Schreibbefehl), `encode(&self) -> [u8; 31]`, `data_len()`, `dir_in()`; `pub enum CswStatus { Passed, Failed, PhaseError }`, `pub fn parse_csw(b: &[u8], tag: u32) -> Result<(CswStatus, u32 residue), MscError>`; `pub fn parse_read_capacity10(b: &[u8]) -> Result<(u32 last_lba, u32 block_len), MscError>` (Blocklänge 512..=4096, Zweierpotenz).
- Produces (`bootmark`): `pub const PLACEHOLDER: [u8; 32]` (`b"L_OS-BOOT-ID-PLACEHOLDER-v1\0\0\0\0\0"`), `pub type BootId = [u8; 16]`, `pub fn find_placeholder(kernel: &[u8]) -> Result<usize, MarkError>` (`Missing`, `Duplicate`), `pub fn compute_id(kernel: &[u8], at: usize) -> BootId` (FNV-1a-128, Platzhalterbereich als Nullen gerechnet), `pub fn embedded(id: &BootId) -> [u8; 32]` (`b"L_OSID01"` + ID + Nullen), `pub fn parse_embedded(b: &[u8; 32]) -> Option<BootId>` (Platzhalter ⇒ `None`), `pub fn lba0_marker(id: &BootId) -> [u8; 32]` (`b"L_OSBOOT"` + ID + Nullen, gehört an Offset 0 von LBA 0), `pub fn parse_lba0(sector: &[u8]) -> Option<BootId>`.

- [ ] **Step 1: Failing tests:** `cbw_signature_and_length_31`, `read10_cdb_big_endian`, `read10_data_len_is_blocks_times_len`, `csw_ok`, `csw_wrong_signature_rejected`, `csw_tag_mismatch_rejected`, `csw_short_rejected`, `read_capacity_parse`, `read_capacity_bad_block_len_rejected`; `placeholder_found_once`, `missing_placeholder_reported`, `duplicate_placeholder_rejected`, `id_same_before_and_after_patch`, `embedded_roundtrip_and_placeholder_is_none`, `lba0_roundtrip`, `lba0_without_magic_is_none`, `marker_fits_in_mbr_bootstrap_area` (32 ≤ 440).
- [ ] **Step 2:** `scripts/host_tests.sh usb_msc::` und `bootmark::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh usb_msc::` und `bootmark::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): read-only bulk-only transport and boot marker`.

### Task 17: Image-Builder schreibt den Boot-Marker; Runner kennt USB-Tests

**Files:**
- Modify: `tools/image/Cargo.toml` (`l_core = { path = "../../crates/l_core" }`), `tools/image/Cargo.lock`, `tools/image/src/lib.rs`, `tools/image/src/bin/l_os_run.rs`, `src/lib.rs` (`#[used] #[unsafe(link_section = ".rodata.l_os_bootid")] pub static BOOT_ID: [u8; 32] = l_core::bootmark::PLACEHOLDER;` und `pub fn boot_id() -> Option<BootId>` per `read_volatile`)
- Test: Unit-Tests in `tools/image/src/lib.rs`

**Interfaces:**
- Consumes: `l_core::bootmark` (Task 16).
- Produces: `pub fn patch_kernel(kernel: &[u8]) -> Result<(Vec<u8>, Option<BootId>), String>` (Platzhalter fehlt ⇒ unverändert, `None`; doppelt ⇒ Fehler), `pub fn write_lba0_marker(image: &mut [u8], id: &BootId) -> Result<(), String>` (nur wenn Bytes 0..32 null sind; sonst Fehler, nie überschreiben), `build_image` nutzt beides über eine Temp-Kopie des Kernels; `pub fn usb_test_args(image: &Path) -> Vec<OsString>` (`-device qemu-xhci,id=xhci`, `-drive if=none,id=stick,format=raw,readonly=on,file=…`, `-device usb-storage,bus=xhci.0,drive=stick,id=stick0`, `-device usb-kbd,bus=xhci.0,id=kbd0`) und `pub fn is_usb_test(path: &str) -> bool` (Testname beginnt mit `usb_`); `l_os_run` hängt die Argumente nur für solche Tests an.

- [ ] **Step 1: Failing tests:** `patch_replaces_placeholder_with_embedded_id`, `patch_without_placeholder_is_noop`, `patch_with_two_placeholders_fails`, `lba0_marker_written_at_offset_0`, `lba0_nonzero_bootstrap_is_refused`, `usb_test_detection`, `usb_args_attach_same_image_readonly`, `test_mode_args_unchanged_for_other_tests` (bestehender Test bleibt grün).
- [ ] **Step 2:** `scripts/host_tests.sh` (Teil `tools/image`) → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh` → PASS; `cargo test` → PASS (alle Kernel-Images tragen jetzt den Marker); `python3 -I scripts/qemu_shell_smoke.py` → PASS.
- [ ] **Step 5:** Commit `feat(image): boot id in kernel and LBA0; USB devices for usb_* tests`.

### Task 18: PCI-BARs und uncached MMIO-Fenster

**Files:**
- Create: `crates/l_core/src/pci_bar.rs`, `src/mmio.rs`, `tests/usb_xhci.rs` (erster Test)
- Modify: `crates/l_core/src/lib.rs`, `src/pci.rs` (`prog_if`, `bar64(&Device, idx) -> Bar`, `bar_size(&Device, idx) -> Option<u64>` mit Decoding aus während der Messung), `src/lib.rs`, `Cargo.toml` (`[[test]] name = "usb_xhci"`)

**Interfaces:**
- Produces: `l_core::pci_bar::{Bar, decode(lo: u32, hi: u32) -> Bar, size_from_probe(lo: u32, hi: u32, is64: bool) -> Option<u64>}`; `pci::Device { …, prog_if: u8 }`; `mmio::map(phys: u64, len: u64) -> Result<Mmio, MapError>` (L4-Slot 144, `PRESENT | WRITABLE | NO_CACHE | WRITE_THROUGH | NO_EXECUTE`), `Mmio::read32(off)`, `Mmio::write32(off, v)`, `Mmio::write64(off, v)` (volatile, Grenzprüfung).

- [ ] **Step 1: Failing tests:** Host `pci_bar::`: `decode_io_bar`, `decode_mem32_prefetch`, `decode_mem64_combines_hi`, `size_from_probe_64k`, `size_from_probe_zero_is_none`. Kernel `tests/usb_xhci.rs`: `finds_xhci_by_class` (Klasse 0x0C/0x03/0x30, BAR0 64-bit Memory), `mmio_window_reads_capability_length` (`CAPLENGTH` ≥ 0x20, `HCIVERSION` ≥ 0x0100), `mmio_mapping_is_uncached_and_nx` (Seitentabelleneintrag prüfen).
- [ ] **Step 2:** `scripts/host_tests.sh pci_bar::`, `cargo test --test usb_xhci` → FAIL.
- [ ] **Step 3:** Implementieren; `tests/kernel_mappings.rs` muss weiter grün sein (Fenster ist NX und nicht `Usable`).
- [ ] **Step 4:** `scripts/host_tests.sh`, `cargo test` → PASS.
- [ ] **Step 5:** Commit `feat(pci): 64-bit BARs and an uncached MMIO window`.

### Task 19: xHCI-Controller-Init und Command-Ring

**Files:**
- Create: `src/xhci.rs`
- Modify: `src/lib.rs` (Modul), `tests/usb_xhci.rs`

**Interfaces:**
- Consumes: Tasks 12, 13, 18; `allocate_contiguous`.
- Produces: `pub fn init() -> Result<(), XhciError>` (Spec §7.1 Schritte 1–4, alle Wartezeiten mit Tick-Timeout), `pub fn command(trb: Trb, timeout_ms: u64) -> Result<Event, XhciError>` (nur vor dem ISR-Polling), `pub fn caps() -> Option<Caps>`, `pub fn halt_for_wipe()` (lockfrei: `USBCMD.RS=0`, begrenztes Warten auf `HCH`; Ersatz für den Platzhalter in Task 10).

- [ ] **Step 1: Failing tests** `tests/usb_xhci.rs`: `controller_resets_and_runs` (`USBSTS.HCH == 0`, `CNR == 0` nach `init`), `noop_command_completes` (Completion-Code `SUCCESS`, Zeiger auf das No-Op-TRB), `command_ring_wraps` (70 No-Ops über einen Ring mit 64 Plätzen, alle `SUCCESS`), `halt_for_wipe_stops_controller`.
- [ ] **Step 2:** `cargo test --test usb_xhci` → FAIL.
- [ ] **Step 3:** Implementieren (Doorbell 0 für Commands, Event-Ring-Abfrage über `EventRing::poll`, `ERDP` nach jedem Ereignis).
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5:** Commit `feat(xhci): controller reset, rings and command submission`.

### Task 20: Port-Reset und Enumeration an Root-Ports

**Files:**
- Create: `src/usb.rs`
- Modify: `src/xhci.rs` (Ports, Slots, Control-Transfers auf EP0), `src/lib.rs`, `tests/usb_xhci.rs`

**Interfaces:**
- Consumes: Tasks 13, 14, 19.
- Produces: `xhci::control_in(slot, packet: SetupPacket, buf_phys, len) -> Result<usize, XhciError>`, `xhci::control_out_nodata(slot, packet)`, `xhci::configure_endpoints(slot, eps: &[(Endpoint, Speed)]) -> Result<(), XhciError>`, `usb::enumerate() -> UsbSummary` mit `pub struct UsbDevice { port: u8, slot: u8, speed: Speed, kind: DeviceKind }` (feste Liste, höchstens 8), Ausgabe je Gerät auf der Konsole (`usb: port 5 high-speed massenspeicher`).

- [ ] **Step 1: Failing tests:** `enumerates_keyboard_and_storage` (zwei Geräte; Klassifikation `BootKeyboard` und `BulkOnlyStorage`; Ports wie im Spike notiert), `device_descriptor_vendor_matches_qemu` (Vendor/Product aus der Spike-Notiz), `port_ack_keeps_port_enabled` (nach allen Quittierungen `PED == 1` an beiden Ports).
- [ ] **Step 2:** `cargo test --test usb_xhci` → FAIL.
- [ ] **Step 3:** Implementieren (Spec §7.1 Schritte 5–6; Evaluate Context, wenn `bMaxPacketSize0` von der Speed-Vorgabe abweicht).
- [ ] **Step 4:** `cargo test` → PASS. **Abbruchkriterium** (Spec §7.3): läuft dieser Task nach zwei Review-Runden nicht stabil, Stopp und Notiz.
- [ ] **Step 5:** Commit `feat(usb): port reset and enumeration on root ports`.

### Task 21: Boot-Stick erkennen, versiegeln, Amnesie-Status anzeigen

**Files:**
- Modify: `src/usb.rs` (Bulk-Only-Lesen, Versiegeln), `src/wipe.rs` (`amnesia()` mit `stick_sealed`), `src/aurora.rs` (`STATUS_RIGHT` aus `Amnesia::label`), `crates/l_core/src/syscall.rs` (`SYSINFO_AMNESIA = 3`, `pack_amnesia`/`unpack_amnesia`), `src/syscall.rs`, `user/src/bin/shell.rs` (`security`: Zeilen `amnesisch: ja|nein (<Grund>)`, `usb: tastatur ja|nein, stick: versiegelt|fehlt, stick-lesezugriffe <n>`, `acpi-poweroff: ja|nein`), `scripts/qemu_shell_smoke.py` (erwartete `security`-Zeilen), `src/lib.rs`, `Cargo.toml` (`[[test]] name = "usb_storage"`)
- Create: `tests/usb_storage.rs`

**Interfaces:**
- Consumes: Tasks 3, 9, 16, 17, 20.
- Produces: `usb::identify_boot_stick() -> Result<u8 port, StickError>`, `usb::storage_read(lba, buf) -> Result<(), StorageError>` (`Err(Sealed)` nach `storage_seal()`), `usb::storage_seal()`, `usb::stick_reads() -> u64`, `usb::stick_port() -> Option<u8>`; `l_core::syscall::{SYSINFO_AMNESIA, pack_amnesia(a: Amnesia, keyboard: bool, stick_reads: u32, acpi: bool) -> u64, unpack_amnesia(u64)}`.

- [ ] **Step 1: Failing tests:** Host `syscall::`: `amnesia_pack_roundtrip`. Kernel `tests/usb_storage.rs`: `boot_id_is_patched` (`boot_id()` ist `Some`), `stick_is_identified_by_boot_marker` (Port = Port des `usb-storage`), `reads_refused_after_seal` (`storage_read` → `Err(Sealed)`, `stick_reads()` unverändert), `amnesia_label_reflects_stick_and_mirror` (ohne `serial-console`: `"amnesisch: ja"`). Smoke: Erwartung, dass `security` im Spiegel-Build `amnesisch: nein (Serial-Spiegel an)` zeigt.
- [ ] **Step 2:** `cargo test --test usb_storage`, `scripts/host_tests.sh syscall::`, `python3 -I scripts/qemu_shell_smoke.py` → FAIL.
- [ ] **Step 3:** Implementieren (Spec §7.1 Schritt 7 "Massenspeicher" und Schritt 8; ohne Stick bleibt der Status "nein" und das System läuft, Spec E8).
- [ ] **Step 4:** `scripts/host_tests.sh`, `cargo test`, `python3 -I scripts/qemu_shell_smoke.py` → PASS.
- [ ] **Step 5:** Commit `feat(usb): identify and seal the boot stick; truthful amnesia status`.

### Task 22: USB-Tastatur im Eingabepfad

**Files:**
- Modify: `src/usb.rs` (Boot-Protokoll, Interrupt-Transfers), `src/xhci.rs` (`poll_tick()`: Transfer-Events der Tastatur), `src/interrupts.rs` (Timer-ISR ruft `xhci::poll_tick()` nach EOI, vor `sched::on_tick`), `Cargo.toml` (`[[test]] name = "usb_keyboard"`), `.github/workflows/ci.yml`
- Create: `tests/usb_keyboard.rs`, `scripts/qemu_usb_smoke.py`

**Interfaces:**
- Consumes: Tasks 10 (`input::feed`), 15, 20.
- Produces: `xhci::poll_tick()` (höchstens 16 Ereignisse, nur nach `init`, IF=0), `usb::keyboard_reports() -> u64`; `scripts/qemu_usb_smoke.py` (Build `--features serial-console`, QEMU mit `qemu-xhci`, `usb-kbd` (`id=kbd0`), Stick, QMP).

- [ ] **Step 1: Failing tests:** Kernel `tests/usb_keyboard.rs`: `keyboard_endpoint_running` (Endpoint-Zustand im Output-Kontext = Running, 8 TRBs eingereiht). Skript: `usb_keyboard_types_into_shell` (Text per QMP `send-key` tippen, Ausgabe im Serial-Log; `security` zeigt Berichte > 0), `stick_read_count_constant_after_seal` (`security` vor und nach `run hello`, `run hello_rs`: gleiche Zahl), `keyboard_unplug_does_not_wipe` (QMP `device_del kbd0`, danach antwortet die Shell auf PS/2-Eingaben – dazu wird nach dem Abziehen über PS/2 getippt –, kein `SHUTDOWN`-Ereignis).
- [ ] **Step 2:** `cargo test --test usb_keyboard`, `python3 -I scripts/qemu_usb_smoke.py` → FAIL. Wenn der Spike (Task 1, Frage 5) gezeigt hat, dass `send-key` die USB-Tastatur nicht erreicht: Abspeckregel Spec §7.3 anwenden und das im Commit festhalten.
- [ ] **Step 3:** Implementieren; Tastatur-Trennung markiert das Gerät nur als weg.
- [ ] **Step 4:** `cargo test`, `python3 -I scripts/qemu_usb_smoke.py`, Smoke → PASS; CI-Schritt "USB smoke".
- [ ] **Step 5:** Commit `feat(usb): HID boot keyboard feeds the existing input path`.

### Task 23: Abzieh-Auslöser und Controller-Verlust

**Files:**
- Create: `tests/usb_wipe_hooks.rs` (`[[test]] name = "usb_wipe_hooks"`, `required-features = ["test-hooks"]`)
- Modify: `src/xhci.rs` (`poll_tick`: `USBSTS`-Prüfung, Port-Status-Change → `classify`), `src/usb.rs` (Scharfschalten nach §7.1 Schritt 8), `src/wipe.rs` (Auslöser `StickRemoved`, `ControllerLost`), `Cargo.toml`, `.github/workflows/ci.yml` (Schritt `cargo test --features test-hooks --test usb_wipe_hooks`)

**Interfaces:**
- Consumes: Tasks 10, 13, 21, 22.
- Produces: Wipe bei `PortChange::Disconnected` am Stick-Port oder `ControllerGone`/`HCE`/`HSE`; mit Feature `test-hooks` über den Hook aus Task 10 gezählt statt ausgeführt (`wipe::test_hook_count()`); `xhci::simulate_read_all_ones(bool)` existiert nur mit `test-hooks`.

- [ ] **Step 1: Failing tests** `tests/usb_wipe_hooks.rs`: `disconnect_classification_from_live_portsc` (aktuelles PORTSC des Stick-Ports mit `CCS=1` ergibt `Nothing`, ein künstlich gebildeter Wert mit `CCS=0, CSC=1` ergibt `Disconnected`), `controller_gone_triggers_wipe_hook` (`poll_tick` mit `simulate_read_all_ones(true)`: Hook-Zähler 1), `keyboard_port_change_does_not_trigger` (Hook-Zähler bleibt 0 bei Ereignis am Tastatur-Port).
- [ ] **Step 2:** `cargo test --features test-hooks --test usb_wipe_hooks` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `cargo test`, `cargo test --features test-hooks --test usb_wipe_hooks`, `python3 -I scripts/qemu_usb_smoke.py` (`keyboard_unplug_does_not_wipe` weiter grün) → PASS.
- [ ] **Step 5:** Commit `feat(wipe): wipe on boot-stick removal and on controller loss`.

### Task 24: Abnahme Abzieh-Wipe und Messung der Dauer

**Files:**
- Modify: `scripts/qemu_wipe_test.py` (`--trigger unplug`, `--boot usb|ide` nach Spike-Ergebnis, Messung über mehrere `--mem`), `.github/workflows/ci.yml` (Schritt "Wipe test (unplug)")

**Interfaces:**
- Consumes: Tasks 11, 23.
- Produces: Prüfungen wie Task 11 plus `unplug_triggers_wipe` (QMP `device_del stick0` → `SHUTDOWN` mit `guest-shutdown` innerhalb von 60 s; `WIPE-REPORT` mit `trigger=unplug`); Messtabelle (RAM-Größe, `total_us`, `keys_us`, `user_us`, Host-Zeit, Host-CPU, TCG/KVM).

- [ ] **Step 1: Failing check:** `python3 -I scripts/qemu_wipe_test.py --trigger unplug` vor dem Skript-Update → FAIL (Option unbekannt), nach dem Update gegen einen Kernel mit absichtlich deaktiviertem Port-Auslöser (Mutation) → FAIL `unplug_triggers_wipe`; Mutation zurücknehmen.
- [ ] **Step 2:** `--trigger unplug --mem 256M` → PASS (CI).
- [ ] **Step 3:** Manuell `--mem 1G` und `--mem 4G` (über 4 GiB: belegt Stufe 8 oberhalb der Bitmap-Grenze) für beide Auslöser; Werte, Host-CPU, QEMU-Version, Beschleuniger notieren.
- [ ] **Step 4:** CI-Schritt ergänzen; alles grün.
- [ ] **Step 5:** Commit `test(wipe): unplug acceptance and measured wipe duration`.

### Task 25: Dokumentation und Abschluss M2

**Files:**
- Modify: `docs/ROADMAP.md`, `docs/THREAT_MODEL.md`, `docs/SECURITY.md`, `docs/ARCHITECTURE.md`, `README.md`, `docs/superpowers/specs/2026-10-12-zukunftsplan-design.md` (§5: Messwert statt **[assumed]**)

- [ ] **Step 1:** ROADMAP: M2 unter "Done" mit je einem Beleg pro Aussage (Testname oder Skript); offene Abspeckungen (z. B. Boot von USB nur mit IDE-Rückfall, falls zutreffend) unter "Not done / not verified"; Eingangskriterium M5 (Spiegel aus) als erfüllt markieren.
- [ ] **Step 2:** THREAT_MODEL: Kaltstart-Absatz mit gemessener Dauer (Werte und Umgebung, TCG-Vorbehalt), was genullt wird und was nicht (Spec §8.4), Auslöser, Fenster "Stick vor Erkennung gezogen" (Spec E8), DMA-Stopp, keine IOMMU; Satz "No wipe on unplug … exists yet" ersetzen.
- [ ] **Step 3:** SECURITY: "Honest status" (USB-Treiber im Ring 0 parst Gerätedaten; Massenspeicher nur lesend, versiegelt), Known weaknesses (Hotplug-Enumeration fehlt, keine Hubs, Polling-Latenz, Register/Caches best effort), `unwrap`-Inventar neu erheben (`grep -rn "unwrap()\|expect(\|panic!(" src`).
- [ ] **Step 4:** ARCHITECTURE: Tabellenzeilen USB/xHCI, ACPI, Wipe, Storage ("read-only, sealed after boot"), Serial ("mirror off by default"), Heapgröße korrigieren (4 MiB statt 1 MiB), Modulkarte, Testschichten (neue Skripte). README: Features und Alias.
- [ ] **Step 5:** Nur belegte Aussagen; jede nennt ihren Test. Commit `docs: M2 amnesic live system done`.

---

## Selbstprüfung

- **Spec-Abdeckung:** §1 Abnahme 1 → Task 24; Abnahme 2 → Tasks 11, 24; Abnahme 3 → Tasks 5, 10, 11, 24, 25; Abnahme 4 → Task 2. §4 E1 → 2; E2 → 21 (Versiegeln), 25; E3 → 16, 17, 21; E4 → 19, 22, 23; E5 → 18; E6 → 7, 10, 15; E7 → 3, 21; E8 → 21; E9/E10/E11/E14 → 10; E12 → 2, 10; E13 → 6, 8, 10. §6 → 21, 22. §7.1 → 18–21; §7.2 → 22, 23; §7.3 → 1, 20, 22. §8.1–8.6 → 3–5, 9–11, 24. §9 → Testnamen der Tasks. §10 → Review Focus.
- **Reihenfolge:** Teil A (1–11) ist ohne USB lieferbar und abnahmefähig; Teil B baut nur auf A und den eigenen Vorgängern auf. Jeder Task lässt `scripts/host_tests.sh`, `cargo test` und die Smoke-Skripte grün.
- **Typkonsistenz:** `Stage`/`Trigger`/`Amnesia` (3) in 5, 9, 10, 21, 23; `PhysRange`/`RangeList` (4) in 9, 10; `StageTimes` (5) in 10; `Fadt`/`s5_value` (6) in 8; `HotkeyDetector` (7) in 10, 15; `Trb`/`ProducerRing`/`EventRing` (12) in 19–23; `PortSc`/`classify`/`Speed`/Kontexte (13) in 19–23; `SetupPacket`/`DeviceKind` (14) in 20–22; `report_to_set1` (15) in 22; `Cbw`/`bootmark` (16) in 17, 21; `Bar` (18) in 18, 19.

## Entschiedene offene Punkte

1. Serial-Spiegel: Cargo-Feature mit `default = []`, kein separates Cargo-Profil (Features steuern Code, Profile nur Optimierung); Skripte und Alias schalten es ein (Spec §5). Test-Eingriffe in den Wipe nur über das Feature `test-hooks`, das "amnesisch: ja" ausschließt; kein Laufzeitschalter.
2. "Image im RAM" ist durch den UEFI-Ablauf schon gegeben; M2 belegt nur "Stick nach Boot nicht mehr gelesen" (Versiegeln + Zähler) und behauptet keine Bitgleichheit (Spec §6).
3. Stick-Identifikation über Boot-ID im Kernel und in LBA 0, kein FAT-Parser (Spec E3).
4. xHCI per Polling im Timer-ISR, keine Interrupts (Spec E4).
5. Eigenes uncached MMIO-Fenster statt Offset-Mapping (Spec E5).
6. Hotkey `Strg+Alt+Rücktaste`, im Kernel erkannt (Spec E6).
7. "amnesisch: ja" nur bei versiegeltem Stick, gültigem Plan und ausgeschaltetem Spiegel (Spec E7).
8. Kein Halt, wenn beim Boot kein Stick gefunden wird; Abweichung von Zukunftsplan §5 begründet (Spec E8).
9. Keine "Vergessen"-Animation vor dem Wipe (Spec E9).
10. Code, `.rodata`, RELRO, Kernel-ELF-Kopie, Bootloader-Seitentabellen und Firmware-Bereiche werden nicht genullt; Insel zuletzt per `asm!` (Spec §8.4).
11. Auch Controller-Verlust löst den Wipe aus (fail-closed, Spec §7.2).
12. Hotplug-Enumeration, Hubs, MSI, UAS, Schreiben: außerhalb von M2 (Spec §3).
13. Abbruch-/Abspeckkriterien für OVMF-USB-Boot, USB-Tastatur-Routing und Enumeration festgelegt; der Wipe selbst wird nie abgespeckt (Spec §7.3).
14. Messung unter QEMU mit 256 MiB in CI, 1 GiB und 4 GiB manuell; reale Hardware in M5 (Spec §8.6).
