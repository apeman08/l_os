# M2 "Amnesisches Live-System": Design

Stand: 2026-10-13, Nachträge aus der Umsetzung 2026-10-15 (Abschnitt 11). Status: umgesetzt (Plan-Tasks 1–25). Aussagen sind mit **[assumed]** markiert, wenn nichts sie belegt. Übergeordnete Spec: `docs/superpowers/specs/2026-10-12-zukunftsplan-design.md` (Abschnitte 4, 5, 7, 8). Plan: `docs/superpowers/plans/2026-10-13-m2-amnesisch.md`.

## 1. Ziel und Abnahme

l_os läuft von einem USB-Stick, braucht ihn nach dem Boot nicht mehr und vergisst beim Abziehen des Sticks oder per Notfall-Hotkey alles: Eingabe sperren, Bildschirm schwärzen, Schlüssel, dann Nutzerprozesse, dann alle Frames nullen, dann ACPI-Poweroff, ersatzweise Reset (Zukunftsplan §5).

**Abnahme (aus `docs/ROADMAP.md`, "Next 1"):**
1. QEMU `device_del` des Sticks löst den Wipe aus (`scripts/qemu_wipe_test.py --trigger unplug`).
2. Ein Speicherabbild nach dem Wipe zeigt Schlüssel- und Nutzerspeicher als null; zusätzlich ist der gesamte als `Usable` gemeldete RAM null und der getippte Sentinel-Text kommt im Abbild nirgends vor (gleiches Skript, auch `--trigger hotkey`).
3. Die gemessene Wipe-Dauer ist dokumentiert und ersetzt das **[assumed]** "einige Sekunden" (Zukunftsplan §5, `docs/THREAT_MODEL.md`).
4. Eingangskriterium: Der `serial-console`-Spiegel ist im normalen Image AUS; nur ein ausdrücklich gewähltes Test-/Smoke-Profil schaltet ihn ein.

## 2. Ausgangslage (geprüft am Code, Stand c-prod 7f1f1ab)

- Speicher (`src/memory.rs`): Frame-Allocator mit Free-List, Frames werden bei Vergabe und Freigabe genullt; Bitmap `ALLOCATED` verfolgt nur die ersten 4 GiB. Physischer Speicher liegt vollständig ab `PHYSICAL_MEMORY_OFFSET` = `0x1000_0000_0000` (`src/lib.rs`). Heap 4 MiB ab `0x4444_4444_0000` aus Allocator-Frames (`src/allocator.rs`; `ARCHITECTURE.md` nennt veraltet 1 MiB). Prozess-Kernelstacks (64 KiB) liegen im Heap (`src/sched.rs`). Boot-Kernelstack 2 MiB vom Bootloader.
- `bootloader_api` 0.11.17 `BootInfo` liefert `rsdp_addr`, `kernel_addr`/`kernel_len` (Kernel-ELF im RAM), `kernel_image_offset`, `kernel_stack_bottom`/`kernel_stack_len`, `memory_regions` mit `MemoryRegionKind::{Usable, Bootloader, UnknownUefi, UnknownBios}` (geprüft in der Crate-Quelle `bootloader_api-0.11.17/src/info.rs`).
- Das UEFI-Image (`tools/image`, `bootloader` 0.11.17 `UefiBoot::create_disk_image`) ist eine GPT-Platte mit Protective MBR in LBA 0 (Bootstrap-Bereich Bytes 0..440 ungenutzt) und einer FAT-ESP mit Bootloader und Kernel (geprüft in `bootloader-0.11.17/src/gpt.rs`).
- PCI (`src/pci.rs`): Legacy-Konfigurationszugriff, `Device` ohne Prog-IF, BARs als `u32` (kein 64-Bit-BAR, keine BAR-Größe). Netz: virtio-net legacy über I/O-Ports; es gibt noch kein MMIO-Mapping im Kernel.
- Eingabe (`src/input.rs`, `src/interrupts.rs`): Tastatur-ISR legt rohe Set-1-Scancodes in `SCANCODES` (SPSC-Queue); dekodiert wird in Ring 3 (Shell, `pc-keyboard`, `De105Key`). Es gibt keine kernelseitige Tastenerkennung.
- Serial: Feature `serial-console` ist **default an** (`Cargo.toml`); `fb_console::_print` spiegelt jede Konsolenausgabe nach COM1. `scripts/qemu_shell_smoke.py` und `scripts/qemu_fetch_smoke.py` lesen den Konsolentext aus diesem Spiegel; Kernel-Tests geben über `serial_println!` direkt aus (unabhängig vom Feature).
- Kein Speichertreiber, kein USB, kein ACPI, keine TSC-Nutzung. Statusleiste zeigt fest "amnesisch: nein" (`src/aurora.rs`).
- QEMU-Läufe: Runner `l_os_run` (`tools/image/src/lib.rs`, `qemu_args`): IDE-Boot-Platte `bootindex=0`, `-cpu max`, VGA 1024x768, virtio-net, im Testmodus `isa-debug-exit`, `-display none`, `-no-reboot`; kein `-m` (QEMU-Standard 128 MiB **[assumed]**).

### Werkzeugprüfung in dieser Umgebung (2026-10-13)

| Werkzeug | Ergebnis | Folge |
|---|---|---|
| `cargo --version` | `cargo 1.101.0-nightly (29c5daa1a 2026-10-07)` (Toolchain `nightly-2026-10-11` per `rust-toolchain.toml`, hier frisch installiert); `stable` vorhanden | Host-Tests laufen |
| `cargo +stable test` für `crates/l_core` | 109 passed, 0 failed | `scripts/host_tests.sh` für `l_core` hier lauffähig |
| `openssl` | `/usr/bin/openssl` vorhanden | `l_tls`-Host-Tests prinzipiell lauffähig (hier nicht ausgeführt) |
| `which qemu-system-x86_64` | nicht gefunden | `cargo test`, Smoke-Tests, `qemu_wipe_test.py`, `qemu_usb_smoke.py` laufen hier **nicht**; nur in CI (`ubuntu-24.04`, `apt install qemu-system-x86 ovmf`) oder auf einem Entwicklerrechner |
| OVMF (`/usr/share/ovmf`, `/usr/share/OVMF`, `/usr/share/qemu`) | nicht gefunden | wie oben |

Damit sind in dieser Umgebung nur die reinen Host-Teile (Plan-Tasks 3–7 und 12–16 vollständig; die Host-Tests in Tasks 9, 17 und 18) abnahmefähig. Alles mit QEMU wird in CI abgenommen; die CI-QEMU-Version (Ubuntu 24.04: 8.2 **[assumed]**) muss `-action`, `dump-guest-memory`, `qemu-xhci`, `usb-storage`, `usb-kbd` können (Task 1 prüft das).

## 3. Umfang

**Im Umfang**
- Reine, host-getestete Logik in `crates/l_core` (`#![forbid(unsafe_code)]`): Wipe-Zustandsmaschine und Reihenfolge, Speicherbereichs-Arithmetik des Wipe-Plans, Zeroize-Helfer, Zeitmessungs-Mathematik, Notfall-Hotkey-Erkennung, ACPI-Tabellen (RSDP/RSDT/XSDT/FADT/`_S5`), xHCI-TRBs, Ringe, Kontexte, PORTSC-Dekodierung und Port-Ereignisse, USB-Deskriptoren und Setup-Pakete, HID-Boot-Report zu Set-1-Scancodes, Bulk-Only-Transport und SCSI-Befehle (nur lesend), Boot-Marker, PCI-BAR-Dekodierung.
- Build-Profil: `serial-console` default aus; Test-/Smoke-Pfade schalten es ausdrücklich ein (Abschnitt 5).
- xHCI-Treiber (PCI-Klasse 0x0C/0x03, Prog-IF 0x30) unter QEMU `-device qemu-xhci`: Init, Command- und Event-Ring, Port-Status-Änderungen, Enumeration direkt an Root-Ports, USB-HID-Tastatur (Boot-Protokoll) in den bestehenden Eingabepfad, Massenspeicher (Bulk-Only, SCSI, nur lesend), um den Boot-Stick zu erkennen.
- Wipe bei Abziehen des Sticks, bei Notfall-Hotkey und bei Verlust des Controllers; ACPI-S5-Poweroff, Reset als Rückfall; Messung der Dauer.
- Abnahme-Skripte `scripts/qemu_wipe_test.py`, `scripts/qemu_usb_smoke.py`, CI-Schritte, Doku.

**Ausdrücklich nicht im Umfang (M2)**
- USB-Hubs (auch QEMU `usb-hub`), Geräte hinter Hubs, Route-Strings ≠ 0.
- Enumeration von Geräten, die *nach* dem Boot angesteckt werden (Hotplug-Enumeration). Nach dem Boot werden nur Trennungen ausgewertet; neue Verbindungen werden ignoriert und gezählt. Die Tastatur muss beim Boot stecken.
- MSI/MSI-X und Legacy-INTx des xHCI; mehrere Interrupter; Mehrsegment-Ringe; Streams; isochrone Endpunkte; UAS; mehrere LUNs (nur LUN 0); Schreiben auf irgendeinen Datenträger (es gibt keinen Schreibbefehl im Code).
- Andere HID-Geräte (Maus), HID-Report-Protokoll (nur Boot-Protokoll), LEDs der Tastatur.
- USB 3 SuperSpeed wird in den Kontexten kodiert, aber unter QEMU nicht getestet (`usb-storage` und `usb-kbd` sind USB 2 **[assumed]**); reale Chipsätze und echte Sticks: M5.
- Die "Vergessen"-Animation (Zukunftsplan §6): der Wipe schwärzt sofort, Animation würde ihn verzögern (Entscheidung E9).
- Mehrere CPUs (der Kernel ist Einzelprozessor), IOMMU, Kaltstart-Angriffe über die Wipe-Dauer hinaus, Firmware-/SMM-Speicher.

## 4. Entscheidungen

| # | Entscheidung | Wahl | Begründung |
|---|---|---|---|
| E1 | Serial-Spiegel | `default = []`; `serial-console` nur über `--features serial-console` (Skripte, Cargo-Alias `run-serial`) | Roadmap-Eingangskriterium; ein Feature ist der kleinste Eingriff, den Cargo, CI und Skripte gleich verstehen (Abschnitt 5) |
| E2 | "Image im RAM" | Das gesamte System (Kernel-ELF, eingebettete Ring-3-Programme, Font) ist nach `ExitBootServices` bereits im RAM; der Kernel liest vom Stick nur LBA 0 zur Identifikation, danach ist der Massenspeicherpfad versiegelt | Mit `bootloader` 0.11 lädt die Firmware den Kernel schon in den RAM; es gibt kein Dateisystem und keine Nachlade-Pfade (Abschnitt 6) |
| E3 | Stick-Identifikation | Boot-Marker: `tools/image` patcht einen Platzhalter im Kernel mit einer 128-Bit-Boot-ID und schreibt dieselbe ID in den Bootstrap-Bereich von LBA 0; der Kernel vergleicht | Unabhängig von FAT-/GPT-Parsing; ein Sektor lesen genügt; deterministisch (ID = FNV-1a-128 über das Kernel-ELF mit genulltem Platzhalter). Keine Authentizität (das ist Verified Boot, M6) |
| E4 | Ereignisse des xHCI | Polling des Event-Rings im Timer-Interrupt (100 Hz), höchstens 16 Ereignisse pro Tick; während `xhci::init` pollt die Init selbst | Kein MSI/APIC nötig; PIC-Leitungen 9–11 bleiben maskiert wie heute; Erkennungslatenz ≤ 10 ms plus Abarbeitung |
| E5 | MMIO | Eigenes Kernel-Fenster (L4-Slot 144) mit `NO_CACHE | WRITE_THROUGH | NO_EXECUTE`, nicht über das Physical-Memory-Offset-Mapping | Korrekte Caching-Attribute (M5), funktioniert auch für 64-Bit-BARs oberhalb der vom Bootloader gemappten Grenze |
| E6 | Notfall-Hotkey | `Strg+Alt+Rücktaste` (links oder rechts, AltGr zählt als Alt), ohne Rückfrage, im Kernel erkannt (Tastatur-ISR und HID-Pfad, gemeinsamer Detektor auf Set-1-Scancodes) | Unabhängig von der Ring-3-Shell (die hängen oder kompromittiert sein kann); unwahrscheinliche Fehlbedienung; in QEMU per `sendkey ctrl-alt-backspace` testbar |
| E7 | Wann "amnesisch: ja" | Nur wenn Boot-Stick erkannt und versiegelt, Wipe-Plan gebaut, `serial-console` aus und `test-hooks` aus. Sonst "amnesisch: nein"; Grund im Befehl `security`. Nachtrag: berechnet aus `stick_sealed`, `plan_ready`, Spiegel aus (und `test-hooks` aus über `plan_ready`); seit dem Abschluss-Review ist die scharf geschaltete Stick-Wache Teil von `stick_sealed` (Abschnitt 11) | Die Statusleiste behauptet nichts, was nicht gilt; ein Spiegel-Build kann nie "ja" zeigen |
| E8 | Stick fehlt beim Boot | Kein Halt: System läuft mit "amnesisch: nein"; Hotkey-Wipe bleibt scharf | Abweichung von Zukunftsplan §5 letzter Satz: ohne erkannten Stick existiert noch kein Nutzerzustand, und der Kernel kann "nie gesteckt" (IDE-Boot, alle Kernel-Tests) nicht von "vor der Erkennung gezogen" unterscheiden. Wird der Stick nach der Erkennung gezogen, wird gewipt |
| E9 | Bildschirm beim Wipe | Sofort schwarz (Framebuffer mit Nullen), keine Animation | Sicherheit vor Optik; Animation ist nach M2 optional als Nachlauf denkbar, nie davor |
| E10 | Laufzeitkontext des Wipes | Läuft im Kontext, der den Auslöser erkennt (Tastatur- oder Timer-ISR), kehrt nie zurück, nimmt keine Locks, nutzt keinen Heap | Interrupts sind dort schon aus; unterbrochener Code läuft nie weiter, also kann er keine Sperre mehr freigeben |
| E11 | Eigene Daten des Kernels | Beschreibbare Kernel-Segmente (`.data`/`.bss`) und der Boot-Kernelstack werden genullt; Code und Read-only-Daten nicht (öffentlich, identisch zum Stick); zuletzt nullt ein `asm!`-Block ohne Stack die "Insel" (Wipe-Stack + Plan) und schaltet ab | Abschnitt 8.4 |
| E12 | Testsichtbarkeit | Feature `wipe-test`: füllt Schlüssel-Slot 0 beim Boot aus RDRAND, meldet physische Bereiche und am Ende eine Zeitzeile auf COM1. Ändert keine Wipe-Logik | Das Abnahmeskript muss wissen, wo es prüfen soll; Produktion bleibt auf COM1 still |
| E13 | Abschalten | ACPI S5 über `PM1a_CNT` (+`PM1b_CNT`) mit `SLP_TYPa` aus `\_S5`, vorab bestimmt; Rückfall: `0xCF9` ← 0x06, `0x64` ← 0xFE, dann Triple-Fault, dann `cli; hlt` | Keine ACPI-Bibliothek (AML-Interpreter) nötig; nur das `_S5`-Paket wird gelesen |
| E14 | DMA vor dem Nullen stoppen | Bus-Master-Bit aller PCI-Geräte löschen (lockfreier Konfigurationszugriff) und xHCI anhalten | Sonst schreiben Controller (Tastaturberichte, Netzpakete) nach dem Nullen wieder Nutzerdaten in den RAM |

## 5. Build-Profile und Features

`Cargo.toml`:

```toml
[features]
default = []
# Spiegelt jede Konsolenausgabe nach COM1. NUR für Test-/Smoke-Builds: kopiert alles Getippte und Angezeigte nach außen.
serial-console = []
# Nur für scripts/qemu_wipe_test.py: Schlüssel-Sentinel, physische Bereiche und Wipe-Zeiten auf COM1.
wipe-test = []
# Nur für tests/usb_wipe_hooks.rs: Auslöser werden gezählt statt ausgeführt; schließt "amnesisch: ja" aus.
test-hooks = []
net-tests = []
```

| Pfad | Features | Warum |
|---|---|---|
| `cargo build` / `cargo run` (Standard, Release-Image) | keine | amnesisches Produkt |
| `cargo run-serial` (Alias in `.cargo/config.toml`: `run --features serial-console`) | `serial-console` | Entwickler will Ausgabe im Terminal |
| `cargo test` | keine | Kernel-Tests schreiben über `serial_println!` direkt, nicht über den Spiegel; bleibt ohne Änderung grün (Task 2 belegt das) |
| `scripts/qemu_shell_smoke.py`, `scripts/qemu_fetch_smoke.py`, `scripts/qemu_usb_smoke.py` | `cargo build --features serial-console` | lesen Text aus dem Spiegel |
| `scripts/qemu_wipe_test.py` | `cargo build --features wipe-test` (ohne Spiegel) | prüft zusätzlich, dass der Sentinel nicht im Serial-Log steht |
| `cargo test --features test-hooks --test usb_wipe_hooks` | `test-hooks` | prüft Auslöser-Logik am echten Controller, ohne dass der Test-Kernel sich selbst wipt; kein Laufzeitschalter in normalen Builds |
| CI | alle obigen Aufrufe | unverändert plus neue Schritte |

Der Runner `l_os_run` baut nichts, er bootet nur, was Cargo gebaut hat; "einschalten" heißt daher: der Aufrufer wählt das Feature (Alias oder Skript). Schutz gegen Verwechslung: `aurora`-Status zeigt in Spiegel-Builds nie "amnesisch: ja" (E7), und `security` nennt "Serial-Spiegel an" als Grund. Die Smoke-Prüfung `default_build_has_no_serial_mirror` (in `scripts/qemu_shell_smoke.py`) baut zusätzlich ohne Features, tippt einen Text und verlangt, dass er nicht im Serial-Log steht.

## 6. "Image komplett im RAM": was das heißt und was belegt wird

Mit `bootloader` 0.11 lädt die UEFI-Firmware den Bootloader von der ESP, der Bootloader lädt die Datei `kernel-x86_64` vollständig in den RAM (`BootInfo::kernel_addr`/`kernel_len`) und mappt ihre Segmente; danach ruft er `ExitBootServices`. Alle Ring-3-Programme sind über `build.rs` in den Kernel eingebettet, der Font ebenso; es gibt kein Dateisystem und keine Ramdisk. Das gesamte Image ist also schon beim Kernel-Einsprung im RAM, ohne dass M2 etwas kopieren muss.

"Der Stick ist nach dem Boot nicht mehr nötig" heißt in M2:
1. Der Kernel liest vom Stick genau: `TEST UNIT READY`, `READ CAPACITY(10)`, `READ(10)` von LBA 0 (ein Block). Danach wird der Massenspeicherpfad versiegelt (`usb::storage_seal()`); jeder weitere Lesezugriff liefert `Err(Sealed)` und erzeugt keinen USB-Transfer. Belegt durch den Kernel-Test `usb_storage::reads_refused_after_seal` und die Smoke-Prüfung `stick_read_count_constant_after_seal` (Zähler vor und nach dem Starten aller Programme gleich).
2. Strukturell: kein Codepfad lädt etwas nach (kein Dateisystem, `spawn` nutzt eingebettete ELFs; bestehender Test `tests/userland.rs`). Das ist eine Code-Eigenschaft, kein Test.

Nicht belegt und nicht behauptet: dass der RAM-Inhalt bitgleich zum Stick ist (dafür bräuchte es Verified Boot, M6); dass Firmware nach dem Boot nichts mehr liest (Firmware-Code läuft nach `ExitBootServices` nur über Runtime Services, die der Kernel nicht aufruft).

## 7. xHCI-Treiber: minimale tragfähige Teilmenge

### 7.1 Ablauf
1. **Finden:** `pci::devices()` nach Klasse 0x0C, Subklasse 0x03, Prog-IF 0x30 (erstes Gerät). QEMU `qemu-xhci`: Vendor 0x1b36, Device 0x000d **[assumed]**. BAR0 64-bit dekodieren und Größe bestimmen (Decoding währenddessen aus), im MMIO-Fenster uncached mappen, Bus-Master an.
2. **Reset:** `USBCMD.RS=0`, auf `USBSTS.HCH` warten; `USBCMD.HCRST=1`, warten bis `HCRST=0` und `USBSTS.CNR=0`. Jede Warteschleife mit Tick-Timeout (Standard 500 ms); Timeout ⇒ Treiber aus, Status "kein Boot-Stick".
3. **Datenstrukturen** (alle aus `allocate_contiguous`, also unter 4 GiB und genullt): DCBAA (`MaxSlots+1` Einträge), Scratchpad-Array und -Seiten falls `MaxScratchpadBufs > 0`, Command-Ring (1 Segment, 64 TRBs inkl. Link-TRB mit Toggle-Cycle), Event-Ring (1 Segment, 64 TRBs), ERST mit einem Eintrag; `CONFIG.MaxSlotsEn`, `DCBAAP`, `CRCR` (mit RCS=1), `ERSTSZ`, `ERDP`, `ERSTBA` (in dieser Reihenfolge); `USBCMD.RS=1`. Interrupts bleiben aus (`IMAN.IE=0`, `USBCMD.INTE=0`).
4. **Selbsttest:** No-Op-Command, Command-Completion mit Code Success erwarten.
5. **Ports:** Für jeden Root-Port mit `CCS=1`: auf USB2-Ports Port-Reset (`PR`) und `PRC` abwarten; USB3-Ports sind nach Link-Training freigegeben. Change-Bits quittieren, ohne `PED` zu schreiben (PED ist RW1C: eine 1 schaltet den Port ab).
6. **Enumeration je Gerät:** Enable Slot → Slot-ID; Input-Kontext (Slot: Root-Port, Speed, Route 0, Context Entries 1; EP0: Control, Max Packet nach Speed 8/8/64/512); Address Device; `GET_DESCRIPTOR(Device, 8)`, ggf. EP0-Max-Packet per Evaluate Context korrigieren, dann 18 Byte; `GET_DESCRIPTOR(Configuration)` 9 Byte, dann `wTotalLength` (max. 512 Byte); Klassifikation; Configure Endpoint für die benötigten Endpunkte, **danach** `SET_CONFIGURATION` (Nachtrag: umgekehrt zur ersten Fassung, weil xHCI §4.3.5 Configure Endpoint vor dem Standard-Request verlangt; Task 20). Kontextgröße 32 oder 64 Byte nach `HCCPARAMS1.CSZ`.
7. **Klassen:**
   - *Tastatur* (Interface-Klasse 3, Subklasse 1, Protokoll 1): `SET_PROTOCOL(Boot)`, `SET_IDLE(0)`, Interrupt-IN-Endpunkt, 8 Normal-TRBs à 8 Byte vorab eingereiht. Jede Transfer-Completion: Report mit vorigem vergleichen, Änderungen als Set-1-Scancodes in `input::feed` (gleicher Weg wie PS/2, inklusive Hotkey-Detektor), TRB neu einreihen, Doorbell.
   - *Massenspeicher* (Klasse 8, Subklasse 6, Protokoll 0x50): Bulk-IN/-OUT. CBW/CSW nur für die Whitelist `TEST UNIT READY` (bis zu 5 Versuche), `READ CAPACITY(10)`, `READ(10)`; LUN 0. LBA 0 lesen, Boot-Marker prüfen. Treffer ⇒ dieses Gerät ist der Boot-Stick, sein Root-Port wird gemerkt; danach versiegeln. Kein Treffer ⇒ Gerät wird ignoriert (nie wieder angesprochen).
   - Alles andere: Slot bleibt adressiert, sonst nichts.
8. **Scharf schalten:** ISR-Polling einschalten; danach einmal `PORTSC` des Stick-Ports lesen: `CCS=0` ⇒ Wipe (schließt das Fenster zwischen Erkennung und Polling). Nachtrag (Task 23): ab dem Scharfschalten prüft die Stick-Wache (`watch_check`) in **jedem** Tick lockfrei (Atomics + volatiles `USBSTS`/`PORTSC`, vor `DRIVER.try_lock`), sodass ein belegter Treiber-Lock die Wache nie überspringt.

### 7.2 Laufzeit (Timer-ISR, nach EOI, vor `sched::on_tick`)
- `USBSTS` lesen: `HCE` oder `HSE` gesetzt oder Lesewert `0xFFFF_FFFF` ⇒ Wipe mit Auslöser `ControllerLost` (fail-closed: wer den Stick nicht mehr beobachten kann, wipt). Nachtrag (Task 23, fail-closed): auch `USBSTS.HCH` nach dem Scharfschalten ⇒ `ControllerLost` (ein angehaltener Controller meldet kein Abziehen mehr); `CSC=1` am Stick-Port ⇒ `StickRemoved` auch bei `CCS=1` (schnelles Ab- und Wiederanstecken), unabhängig von Port-Status-Change-Ereignissen (Pegel statt Flanke). Belege: Host `xhci_ctx::watch_controller_loss_wins_and_is_fail_closed`, `watch_stick_disconnect_is_stick_removed_at_first_reading`; Kernel `tests/usb_wipe_hooks.rs`.
- Bis zu 16 Event-TRBs: Port Status Change ⇒ `PORTSC` des Ports lesen, klassifizieren; Trennung am Stick-Port ⇒ Wipe `StickRemoved`; Trennung am Tastatur-Port ⇒ Tastatur als weg markieren (kein Wipe); neue Verbindung ⇒ zählen, ignorieren. Transfer-Event der Tastatur ⇒ wie oben. Danach `ERDP` mit `EHB` schreiben.
- Treiberzustand liegt hinter einem `spin::Mutex`, der nach dem Boot nur noch mit IF=0 genommen wird (Timer-ISR). Der Befehl `security` liest nur Atomics.
- `input::SCANCODES` bekommt einen zweiten Produzenten (Timer-ISR). Beide Produzenten laufen mit IF=0 auf einer CPU, also nie gleichzeitig; der Doc-Kommentar der Queue wird entsprechend geändert.

### 7.3 Abbruch- und Abspeck-Kriterien
- **Spike (Task 1) zeigt, dass OVMF nicht von `usb-storage` an `qemu-xhci` bootet:** Abnahme läuft mit IDE-Boot und derselben Image-Datei zusätzlich als `usb-storage` (readonly). Der Wipe-Auslöser wird identisch geprüft; "Boot vom USB-Stick" gilt dann als nicht unter QEMU belegt und wandert nach M5.
- **`sendkey`/QMP-Eingaben erreichen `usb-kbd` nicht:** USB-Tastatur gilt nur als bis zum Report-Dekoder belegt (Host-Tests, Kernel-Test "Endpunkt läuft"); Abnahme der Eingabe bleibt PS/2. Dokumentiert, nicht als erledigt markiert.
- **Enumeration (Task 20) läuft nach zwei Review-Runden nicht stabil:** Stopp der USB-Tasks; geliefert wird der Hotkey-Wipe (Tasks 1–11, eigenständig abnahmefähig), Status bleibt "amnesisch: nein", Notiz mit Befund. Erst dann über Abspecken entscheiden (z. B. Stick nur als "einziges BBB-Gerät beim Boot" erkennen, ohne Marker-Lesen; schwächer, ausdrücklich zu dokumentieren).
- Der Wipe selbst und seine Speicherabbild-Abnahme werden nicht abgespeckt.

## 8. Wipe

### 8.1 Auslöser
`Trigger::{StickRemoved, Hotkey, ControllerLost, Panic}` (`Panic` nachgetragen, §11). Der erste Auslöser gewinnt; spätere werden ignoriert (atomarer Zustand). Alle Auslöser führen denselben Ablauf aus (Zukunftsplan §5.6).

### 8.2 Stufen (Reihenfolge ist Test-Gegenstand, `l_core::wipe::Stage::ORDER`)

| # | Stufe | Inhalt |
|---|---|---|
| 0 | `Armed` | Plan liegt fertig in der Insel |
| 1 | `InputLocked` | `cli`, Flag `WIPING` (ISRs verwerfen Eingaben), alle PIC-Leitungen per Port-Zugriff maskieren (ohne `PICS`-Lock) |
| 2 | `ScreenBlanked` | Framebuffer komplett mit Nullen (Basis/Länge aus Atomics, ohne Konsolen-Lock); schwarz in RGB und BGR |
| 3 | `DmaStopped` | `USBCMD.RS=0` (kurz auf HCH warten, begrenzt), Bus-Master-Bit aller aufgezählten PCI-Geräte löschen (lockfreier Konfigurationszugriff) |
| 4 | `StackSwitched` | `CR3` = Kernel-L4, Stack auf den Insel-Stack (16 KiB) |
| 5 | `KeysZeroed` | Schlüssel-Slots (`src/secrets.rs`, 4 Slots à 4 KiB in `.bss`) |
| 6 | `UserZeroed` | jeder Frame mit gesetztem Bit in der Nutzer-Bitmap `USER_FRAMES` (Leaf-Frames von Prozessen) |
| 7 | `AllocatedZeroed` | jeder in `ALLOCATED` belegte Frame (Heap inkl. Konsolenraster und Prozess-Kernelstacks, Seitentabellen, DMA-Ringe) |
| 8 | `UsableZeroed` | alle übrigen Frames aller `Usable`-Regionen, auch oberhalb 4 GiB (frei, nie vergeben) |
| 9 | `KernelZeroed` | beschreibbare Kernel-Segmente (`.data`/`.bss`, physisch über Seitentabellen aufgelöst) und Boot-Kernelstack, jeweils ohne die Insel und ohne RELRO (8.4) |
| 10 | `Reported` | nur `wipe-test`: Zeitzeile über lockfreien UART-Zugriff |
| 11 | `PowerOff` | `asm!` ohne Stack: Insel nullen (`rep stosq`), `wbinvd`, S5 schreiben |
| 12 | `ResetFallback` | `0xCF9`, `0x64`, Null-IDT + `int3` (Triple-Fault) |
| 13 | `Halted` | `cli; hlt`-Schleife |

Die Stufen 5–8 erfüllen Zukunftsplan §5.4 (Schlüssel, Nutzerprozesse, alle freien und belegten Frames); 9 und 11 gehen darüber hinaus. "Tor-Zustand" existiert in M2 nicht; Tor (M3) muss seine Schlüssel in `secrets`-Slots halten, um Stufe 5 zu bekommen (Vorgabe für M3). In der Produktion sind die Slots in M2 leer; TLS-Schlüssel von `fetch` liegen in Prozessspeicher (Stufe 6).

### 8.3 Nullen über das Physical-Memory-Offset-Mapping
Frames werden über `PHYSICAL_MEMORY_OFFSET + phys` mit `core::ptr::write_bytes` (Kernel, `unsafe` mit `// SAFETY:`) genullt. Das ist zulässig, weil (a) die Mapping-Tabellen des Offset-Bereichs, der Kernel-Code, der Insel-Stack und die Kernel-L4 Bootloader-Speicher sind (nicht `Usable`, also nicht in Stufen 6–8; im Spike und durch den Kernel-Test `wipe_plan::island_and_code_not_in_usable` geprüft), (b) ab Stufe 4 keine Prozess-Seitentabelle aktiv ist und (c) der Wipe-Code ab Stufe 6 weder Heap noch Heap-Mappings noch das MMIO-Fenster berührt (deren Seitentabellen liegen in Allocator-Frames und werden in Stufe 7 genullt). Der Plan (alle Bereichslisten, PM1-Port und -Wert, Framebuffer-Basis, Insel-Grenzen, TSC-Frequenz) wird beim Scharfschalten **vorab** berechnet und als feste Arrays in der Insel abgelegt; während des Wipes wird nichts allokiert und keine Firmware-Tabelle gelesen. Läuft eine Liste über, ist der Plan ungültig und das System zeigt "amnesisch: nein".

Die Nutzer-Bitmap `USER_FRAMES` (wie `ALLOCATED`, 4 GiB, aber aus `AtomicU64`-Wörtern, damit der Wipe sie ohne Lock lesen kann) wird beim Mappen einer Nutzerseite gesetzt und in `destroy_address_space` gelöscht. Ein Frame oberhalb 4 GiB wird nie vergeben (bestehende Regel), deshalb genügt die Bitmap für Stufe 6.

### 8.4 Wie der Kernel seinen eigenen Speicher nullt
- **Code und Read-only-Daten** (`.text`, `.rodata` inkl. eingebetteter Programme und Font) und die vom Bootloader geladene Kernel-ELF-Kopie werden nicht genullt: sie sind öffentlich und identisch zum Inhalt des Sticks, enthalten also keinen Nutzerzustand.
- **Beschreibbare Segmente** findet `wipe::arm` über die `PT_LOAD`-Header mit `PF_W` der Kernel-ELF im RAM (`kernel_addr`) plus `kernel_image_offset`. Der bestehende Parser `l_core::elf::parse` nimmt nur `ET_EXEC` in einem Fenster an; der Kernel ist ein statisches PIE (`ET_DYN`), daher kommt `l_core::elf::load_segments` hinzu (beide Typen, kein Fenster, liefert auch den `PT_GNU_RELRO`-Bereich). Jede Seite wird über die Kernel-Seitentabellen in eine physische Adresse übersetzt, zusammenhängende Läufe werden zusammengefasst. Ebenso der Boot-Kernelstack (`kernel_stack_bottom`, `kernel_stack_len`).
- **RELRO bleibt stehen:** Der `PT_GNU_RELRO`-Bereich (`.data.rel.ro`, `.got`: beim Laden relozierte Zeiger, danach unverändert, keine Nutzerdaten) wird aus Stufe 9 herausgerechnet, weil `core::fmt` (Stufe 10) und jeder Aufruf über eine vtable dort lesen. Der Wipe-Code darf ab Stufe 9 keine `static` aus `.data`/`.bss` außer der Insel benutzen.
- **Insel:** ein seitenausgerichtetes `static` in `.bss` mit Wipe-Stack (16 KiB) und Plan. Ihre physischen Seiten werden aus Stufe 9 herausgerechnet (`RangeList::subtract`; physisch nicht zusammenhängend erlaubt). Stufe 11 ist ein einziger `asm!`-Block mit `options(noreturn)`, der nur Register benutzt: Insel über ihre *virtuelle* Adresse mit `rep stosq` nullen, `wbinvd`, `out` auf `PM1a_CNT` (und `PM1b_CNT`), kurze Warteschleife, Reset-Rückfall, `cli; hlt`.
- Nach Stufe 9 sind IDT, GDT und TSS genullt. Eine Ausnahme danach endet als Triple-Fault, also Reset: das ist sicherheitlich unkritisch (der Rechner hält an), wird im Test aber als Fehler erkannt (QMP meldet `guest-reset` statt `guest-shutdown`).

**Nicht genullt, mit Begründung:** Code, Read-only-Daten, RELRO und die Kernel-ELF-Kopie (öffentlich bzw. nur relozierte Zeiger); Seitentabellen und Boot-Info des Bootloaders (Adressen, keine Nutzerdaten); UEFI-/ACPI-Firmwarebereiche (vom Kernel nie beschrieben); interne Puffer von Geräten und von QEMU; CPU-Register und Caches über `wbinvd` hinaus (best effort, `docs/SECURITY.md`-Regel "keine Garantien behaupten"). Daten in nicht-`Usable`-Bereichen, die Firmware vor dem Kernel geschrieben hat, sind kein Nutzerzustand.

### 8.5 ACPI-Poweroff
`acpi::init` (beim Boot): RSDP aus `BootInfo::rsdp_addr` (Prüfsumme, Revision ≥ 2 ⇒ XSDT bevorzugt), Tabelle `FACP` suchen (Prüfsummen), `PM1a_CNT_BLK`/`PM1b_CNT_BLK` (bei 0 aus `X_PM1a_CNT_BLK`, nur System-I/O-Adressraum), DSDT (`X_DSDT` vor `DSDT`), darin `_S5_` gefolgt von `PackageOp` suchen und `SLP_TYPa`/`SLP_TYPb` lesen (Byte-Präfix 0x0A, `ZeroOp`, `OneOp`). Wert = `SLP_TYPx << 10 | SLP_EN (1 << 13)`. Ist `SCI_EN` in `PM1a_CNT` nicht gesetzt und `SMI_CMD` ≠ 0, wird beim Boot `ACPI_ENABLE` an `SMI_CMD` geschrieben (begrenztes Warten). Ohne verwertbares Ergebnis fehlt nur der Poweroff, der Reset-Rückfall bleibt; der Plan ist trotzdem gültig, `security` meldet "ACPI-Poweroff: nein". QEMU-Werte (i440fx, `_S5` = 0) **[assumed]**, im Kernel-Test `acpi::finds_s5_in_qemu_tables` belegt.

### 8.6 Messung der Wipe-Dauer
- Kernel: TSC wird beim Boot gegen 5 PIT-Ticks kalibriert (`l_core::wipetime::tsc_hz`); beim Auslösen und nach jeder Stufe wird `rdtsc` in der Insel notiert. `wipe-test` gibt vor Stufe 11 eine Zeile aus: `WIPE-REPORT trigger=<t> keys_us=<n> user_us=<n> allocated_us=<n> usable_us=<n> kernel_us=<n> total_us=<n> bytes=<n>` (Zeit jeweils ab Auslöser).
- Host: Zeit zwischen QMP-Befehl (`device_del` bzw. `send-key`) und QMP-Ereignis `SHUTDOWN` (Zeitstempel der Ereignisse); enthält Erkennungslatenz (≤ 10 ms Polling).
- Gemessen wird mit `-m 256M` (CI) und manuell mit `-m 1G` und `-m 4G` (über die 4-GiB-Grenze); dokumentiert werden Werte, Host-CPU, TCG/KVM. Unter TCG sagen die Werte wenig über echte Hardware; reale Messung ist M5. Die Formulierung "einige Sekunden" wird durch Messwerte mit diesen Einschränkungen ersetzt.

## 9. Abnahmetests

| Behauptung | Beleg | Hier lauffähig? |
|---|---|---|
| Stufenreihenfolge entspricht §5 | `scripts/host_tests.sh wipe::` (`stage_order_matches_spec_section_5`) | ja |
| Plan deckt alle `Usable`-Bytes ab, Insel ausgenommen | `scripts/host_tests.sh memrange::`, Kernel-Test `tests/wipe_plan.rs` | Host ja, Kernel nein (CI) |
| Serial-Spiegel default aus | `python3 -I scripts/qemu_shell_smoke.py` (`default_build_has_no_serial_mirror`) | nein (CI) |
| Hotkey löst Wipe aus; danach Schlüssel-Slot, `Usable`-RAM null, Sentinel nirgends, Bildschirm schwarz, Poweroff statt Reset | `python3 -I scripts/qemu_wipe_test.py --trigger hotkey` | nein (CI) |
| `device_del` des Sticks löst Wipe aus (gleiche Prüfungen) | `python3 -I scripts/qemu_wipe_test.py --trigger unplug` | nein (CI) |
| Abziehen der Tastatur löst keinen Wipe aus | `python3 -I scripts/qemu_usb_smoke.py` (`keyboard_unplug_does_not_wipe`) | nein (CI) |
| USB-Tastatur speist die Shell | `qemu_usb_smoke.py` (`usb_keyboard_types_into_shell`) | nein (CI) |
| Stick nach Boot nicht mehr gelesen | Kernel-Test `usb_storage::reads_refused_after_seal`, `qemu_usb_smoke.py` (`stick_read_count_constant_after_seal`) | nein (CI) |
| Wipe-Dauer | `WIPE-REPORT`-Zeile, Werte in `ARCHITECTURE.md`/`THREAT_MODEL.md` | nein (CI/manuell) |

Das Abnahmeskript startet QEMU mit `-action reboot=shutdown,shutdown=pause` (QEMU ≥ 6.0 **[assumed]** in CI vorhanden), damit nach Poweroff oder Reset der RAM für `dump-guest-memory` stehen bleibt, und wertet die ELF-Core-Datei (PT_LOAD je RAM-Block) aus.

## 10. Risiken

| Risiko | Auswirkung | Gegenmaßnahme |
|---|---|---|
| OVMF bootet nicht von `usb-storage` | Boot vom Stick in QEMU unbelegt | Spike Task 1; Rückfall IDE + identisches Stick-Image (7.3) |
| Wipe-Code berührt nach Stufe 6 den Heap | Triple-Fault, Reset statt Poweroff | Review-Fokus; Executor ohne `alloc`, Plan in der Insel; Test erkennt `guest-reset` |
| Stufe 9 nullt etwas, das der Wipe-Code noch liest (`.data`-Statics, GOT) | Absturz vor Poweroff | RELRO ausgenommen, Executor nutzt ab Stufe 9 nur Insel, Register, Code, `.rodata`; Test erkennt `guest-reset` |
| `BOOT_ID`-Platzhalter wird vom Compiler konstant gefaltet | Kernel sieht nie die gepatchte ID | Zugriff nur per `read_volatile` auf ein `#[used] static`; Kernel-Test `usb_storage::boot_id_is_patched` |
| PORTSC-Schreibzugriff schaltet Port ab (PED) | Stick "verschwindet", Wipe ohne Abziehen | Host-Test `ack_never_writes_ped` |
| Controller schreibt nach dem Nullen per DMA | Tastenanschläge wieder im RAM | Stufe 3 vor Stufe 6; Abbild-Prüfung sucht Sentinel |
| Speicher > 4 GiB | nicht genullt | Stufe 8 nach Speicherkarte, nicht nach Bitmap; manuelle Messung mit `-m 4G` |
| TCG-Zeiten nicht übertragbar | falsche Erwartung | Messwerte immer mit Umgebung angeben; M5 misst real |
| Umfang des xHCI | M2 zieht sich | Abbruchkriterien 7.3; Hotkey-Wipe vorher eigenständig lieferbar |

## 11. Nachträge aus der Umsetzung (Task 25)

Belege und Grenzen im Einzelnen: `docs/ROADMAP.md` (M2), `docs/THREAT_MODEL.md` ("Amnesic wipe"), Protokoll `docs/superpowers/notes/2026-10-14-m2-sdd-ledger.md`.

- **§7.1 Schritt 6:** Configure Endpoint vor `SET_CONFIGURATION` (siehe dort; Task 20).
- **§7.1 Schritt 8 / §7.2:** fail-closed-Erweiterungen der Stick-Wache: `USBSTS.HCH` nach dem Scharfschalten ⇒ `ControllerLost`; `CSC=1` bei `CCS=1` am Stick-Port ⇒ `StickRemoved`; `watch_check` lockfrei in jedem Tick (Task 23).
- **E7:** "amnesisch: ja" setzt seit der Fix-Welle nach dem Abschluss-Review die scharf geschaltete Stick-Wache voraus: der Eingang `stick_sealed` von `l_core::wipe::Amnesia` ist im Kernel `usb::stick_sealed() && xhci::watched_port().is_some()` (kein neues Feld, das Syscall-Format bleibt; ein versiegelter, aber nicht bewachter Stick meldet "kein Boot-Stick"). Belege: `tests/usb_storage.rs::amnesia_label_reflects_stick_and_mirror`, `tests/usb_wipe_hooks.rs`.
- **§1 Abnahme 3 / §8.6:** Messwerte in `docs/superpowers/notes/2026-10-14-m2-wipe-measurements.md` (TCG); übernommen in Zukunftsplan §5 und `docs/THREAT_MODEL.md`.
- **§7.3:** kein Abbruchkriterium ausgelöst: OVMF bootet von `usb-storage` (Spike, Frage 2), `send-key` erreicht `usb-kbd`, Enumeration stabil.
- **§8.2 Stufe 6:** `UserZeroed` ist für die Abbild-Prüfungen redundant zu `AllocatedZeroed` (Nutzer-Frames sind belegte Frames); ihr Zweck (Nutzerdaten zuerst) ist nicht getestet (Task 11).
- **Wipe ohne Plan** nullt keinen RAM (nur Schlüssel-Slots, Bildschirm, DMA-Stopp, Poweroff); **Panik während des Wipes** schaltet sofort ab, RAM kann teilweise ungenullt bleiben (Task 10).
- **§8.1, Panik außerhalb eines Wipes** (Abschluss-Review, fail-closed): Syscalls und ISRs laufen mit IF=0; ein Kernel, der dort in Panik ging, hielt bisher ohne Interrupts an, Stick-Wache und Hotkey waren tot, der RAM blieb erhalten. Jetzt ruft der Panik-Handler (`src/main.rs`) nach der Ausgabe der Meldung `wipe::trigger(Trigger::Panic)` auf (`trigger=panic` im `WIPE-REPORT`): mit Plan der volle Wipe, ohne Plan der eingeschränkte (Entscheidung: auch dann, damit wenigstens Schlüssel-Slots und Bildschirm gelöscht werden; die Meldung bleibt nur auf COM1). Nicht in `cfg(test)`- und `test-hooks`-Builds. Abnahme: `scripts/qemu_wipe_test.py --trigger panic` (F12 löst im `wipe-test`-Build eine Panik im Tastatur-ISR aus; Prüfung `panic_triggers_wipe` und alle Abbild-Prüfungen).
