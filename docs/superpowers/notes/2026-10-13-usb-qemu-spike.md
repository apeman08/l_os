# USB/QEMU-Spike für M2 (Task 1 des Plans `2026-10-13-m2-amnesisch.md`)

Datum: 2026-10-13. Prüft die QEMU-/OVMF-Annahmen der Spec `2026-10-13-m2-amnesisch-design.md` (§6 bis §8), bevor Code darauf baut.

Umgebung (Messung, nicht CI): Ubuntu 24.04.5, `qemu-system-x86_64` 8.2.2 (`1:8.2.2+ds-0ubuntu1.18`), OVMF `2024.02-2ubuntu0.10` (`apt`), **TCG, kein KVM** (`/dev/kvm` fehlt), 4 Host-Kerne (Xeon 2,1 GHz), Kernel-Toolchain `nightly-2026-10-11`. Wegwerf-Code (eine Kopie des Repos mit ersetzter `kernel_main`, ein Python-QMP-Treiber) lag außerhalb des Repos und ist **nicht archiviert**; die Belege unten sind die beim Lauf beobachteten Ausgaben, die Befehle zeigen, wie sie entstanden. Was nicht ausgeführt wurde, ist mit **[nicht geprüft]** markiert.

Der Wegwerf-Kernel (`bootloader` 0.11.17 wie `main`, `BOOTLOADER_CONFIG` unverändert) liest nach dem Boot: `memory_regions`, Seitentabellen-Übersetzungen, ACPI (RSDP, XSDT, FACP, DSDT), PCI, die xHCI-Capability-Register und alle `PORTSC`; er setzt den Controller selbst zurück, legt Command-/Event-Ring an, startet ihn (`RS=1`), sendet ein No-Op und pollt Event-Ring, `PORTSC` und die PS/2-Scancode-Queue. Tasten steuern ihn: `p` schreibt `SLP_TYPa<<10|SLP_EN` auf `PM1a_CNT` (S5), `r` schreibt `0xCF9 <- 6`, `t` lädt eine Null-IDT und löst `int3` aus (Triple-Fault), `s` gibt Adresse des Sentinels aus.

## Kurzfassung (was den Plan ändert)

1. **Boot von `usb-storage` an `qemu-xhci` funktioniert** (bios-OVMF und gesplittetes `OVMF_CODE_4M`/`OVMF_VARS_4M`, `readonly=on` und `snapshot=on`). **Der IDE-Rückfall (Spec §7.3) ist nicht nötig.** Das Plan-Flag `--boot usb|ide` bleibt als Option, Standard ist `usb`.
2. **`send-key` erreicht die USB-Tastatur, nicht PS/2, sobald `usb-kbd` angeschlossen ist** (24 Tasten-Ereignisse: 0 PS/2-Scancodes, 8 x `hid_kbd_queue_full`). Nach `device_del kbd0` geht `send-key` wieder an PS/2. Folge: Tests mit Tasten ohne `usb-kbd` (Task 11) bleiben PS/2; jeder Test mit `usb-kbd` setzt einen funktionierenden HID-Treiber voraus.
3. **`device_del` des Sticks erzeugt ein Port-Status-Change-Event (TRB-Typ 34)** und `PORTSC = 0x000202a0` (`CCS=0`, `CSC=1`, `PED=0`).
4. **Der xHCI-BAR liegt oberhalb 4 GiB** (64-bit BAR0 bei `0xC000000000`, 16 KiB). Das Physical-Memory-Mapping des Bootloaders deckt ihn ab (2-MiB-Seiten, cachebar, kein `NO_CACHE`), das eigene uncached Fenster (Spec E5) muss aber 64-bit-Adressen verkraften; "MMIO unter 4 GiB" stimmt hier nicht.
5. **Das FADT ist Revision 1, 116 Byte lang**: `X_DSDT` und `X_PM1a_CNT_BLK` existieren nicht; ein Lesen an Offset 140 liefert Müll hinter der Tabelle (hier zufällig der DSDT-Wert). Spec §8.5 muss die Tabellenlänge prüfen.
6. **`_S5` = `Package{0,0,0,0}`, also `SLP_TYPa = 0`; `PM1a_CNT` = `0xB004`, `SCI_EN` ist schon gesetzt.** `outw(0xB004, 0x2000)` ergibt sofort `SHUTDOWN` mit `reason: guest-shutdown`. `0xCF9`-Reset und Triple-Fault ergeben `guest-reset`.
7. **Boot-Kernelstack, `.bss`, `.data`, Kernel-ELF-Kopie und alle Bootloader-Seitentabellen sind `Bootloader`, nie `Usable`** (Frage 9). Stufe 9 der Spec ist nötig und reicht für die `Usable`-Bereiche nicht.
8. **Das Speicherabbild enthält Nicht-RAM-`PT_LOAD`s** (VGA-VRAM, Flash, Legacy-Fenster); der Test muss nach `p_paddr` und `Usable`-Bereichen auswerten.

## Befehle und Belege je Frage

### 1. QEMU-Version in CI
```
$ qemu-system-x86_64 --version | head -1
QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18)
$ apt-cache policy qemu-system-x86 ovmf | grep -E "^[a-z]|Candidate"
qemu-system-x86:  Candidate: 1:8.2.2+ds-0ubuntu1.18
ovmf:             Candidate: 2024.02-2ubuntu0.10
$ qemu-system-x86_64 -device help | grep -E "xhci|usb-kbd|usb-storage"
name "nec-usb-xhci", bus PCI
name "qemu-xhci", bus PCI
name "usb-storage", bus usb-bus
name "usb-kbd", bus usb-bus
```
**[nicht geprüft]** in CI selbst: `.github/workflows/ci.yml` installiert `qemu-system-x86 ovmf` per `apt` auf `ubuntu-24.04`; dieselbe Ubuntu-24.04-Quelle liefert hier 8.2.2. Eine spätere Sicherheitsaktualisierung (`8.2.2+ds-0ubuntu1.N`) ist möglich. Der erste CI-Lauf von Task 11 soll `qemu-system-x86_64 --version` ausgeben (Hinweis für Task 11, Schritt 3).

### 2. Bootet OVMF das Image als `usb-storage` mit `readonly=on`?
Ja. Image gebaut mit `l_os_image target/x86_64-l_os/debug/l_os target/l.img` (12 648 448 Byte). QEMU (nur USB-Medium, kein anderes Boot-Gerät, daher beweist der Start den USB-Boot):
```
qemu-system-x86_64 -m 256M -cpu max -display none -vga none -device VGA,xres=1024,yres=768 \
  -serial file:S.serial -qmp unix:S.qmp,server=on,wait=off -action reboot=shutdown,shutdown=pause \
  -bios /usr/share/qemu/OVMF.fd \
  -drive if=none,id=stick,format=raw,file=l.img,readonly=on \
  -device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=stick,id=stick0,bootindex=0
```
Serial-Log (OVMF-Debug auf COM1):
```
BdsDxe: loading Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:04.0-1" from PciRoot(0x0)/Pci(0x4,0x0)/USB(0x0,0x0)
BdsDxe: starting Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:04.0-1" from PciRoot(0x0)/Pci(0x4,0x0)/USB(0x0,0x0)
Larp OS 0.1.0 - shell running in ring 3
```
Varianten, alle bis zum Kernel gebootet:

| Variante | Ergebnis |
|---|---|
| `-bios OVMF.fd`, `usb-storage`, `readonly=on` | bootet |
| `-bios OVMF.fd`, `usb-storage`, `snapshot=on` | bootet |
| Split-Paar `OVMF_CODE_4M.fd` (pflash, ro) + `OVMF_VARS_4M.fd` (`snapshot=on`) | bootet, gleiche Boot-Zeilen |
| IDE (`-device ide-hd,...,bootindex=0`) | bootet (`Ata(Secondary,Master)`), `qemu-xhci` ohne Gerät: alle `CCS=0` |
| IDE und gleiches Image zusätzlich als `usb-storage` | bootet von IDE, Stick ist an Port 1 sichtbar (`CCS=1`, `PED=1`) |

Nebenbefund: `ide-hd` lehnt `readonly=on` ab (`Block node is read-only`); für einen IDE-Boot (Rückfall, nicht nötig) muss `snapshot=on` verwendet werden (so macht es der Runner). `usb-storage` akzeptiert `readonly=on`.

Bildschirmfoto (`screendump` des echten Kernels, gebootet vom `usb-storage`, 1024x768 PPM, 5 verschiedene Farben): Kopfzeile "Larp OS", rechts "amnesisch: nein", Zeilen `Larp OS 0.1.0 - shell running in ring 3`, `Type 'help' for a list of commands.`, Prompt `l_os>`, Hinweisleiste "Enter ausführen  Backspace löschen  help Befehle". Das Bild ist nicht archiviert (Spike-Regel); der Befehl: QMP `{"execute":"screendump","arguments":{"filename":"real.ppm"}}`.

### 3. PCI-IDs, BAR0, Capability-Register von `qemu-xhci`
Ausgabe des Wegwerf-Kernels (`-device qemu-xhci,id=xhci`, 256 MiB):
```
pci 00:04.0 1b36:000d class=0c03 progif=30 rev=01 bar0=0x4 cmd=0x7
xHCI BAR0 raw=0x00000004 BAR1 raw=0x000000c0: io-space=0 type=2 (0=32bit,2=64bit) prefetchable=0
xHCI BAR size probe: mask lo=0xffffc004 hi=0xffffffff -> size=0x4000 (16 KiB)
xHCI BAR0 base=0xc000000000 (below 4GiB? false)
xHCI dword0 @cap = 0x01000040 (HCIVERSION = dword0>>16 = 0x0100); 16-bit read @cap+2 = 0x0
xHCI CAPLENGTH=64 HCIVERSION=0x0 HCSPARAMS1=0x08001040 (MaxSlots=64 MaxIntrs=16 MaxPorts=8) HCSPARAMS2=0x0000000f HCSPARAMS3=0x00000000
xHCI HCSPARAMS2: IST=15 ERSTMax(2^n)=0 MaxScratchpadBufs=0 SPR=0
xHCI HCCPARAMS1=0x00087001: AC64=1 BNC=0 CSZ=0 (ctx 32 bytes) PPC=0 PIND=0 LHRC=0 NSS=0 xECP=0x8
xHCI DBOFF=0x2000 RTSOFF=0x1000 HCCPARAMS2=0x0
xHCI ext cap SupportedProtocol: USB 2.00 name="USB " ports offset=5 count=4 psic=0
xHCI ext cap SupportedProtocol: USB 3.00 name="USB " ports offset=1 count=4 psic=0
xHCI USBCMD=0x0 USBSTS=0x9 (before our reset; firmware state)
```
Folgerungen: Vendor/Device `0x1b36:0x000d` bestätigt (Spec §7.1 Schritt 1: **[assumed]** entfällt). BAR0 ist 64-bit, nicht prefetchable, **bei `0xC000000000` (oberhalb 4 GiB)**, 16 KiB; OVMF hat ihn zugewiesen, Memory- und Bus-Master-Bit stehen beim Kernel-Einsprung (`cmd=0x7`). `MaxSlots` 64, `MaxIntrs` 16, `MaxPorts` 8, `MaxScratchpadBufs` 0 (der Scratchpad-Pfad wird in QEMU nie ausgeführt, bleibt ungetestet), `AC64=1`, `CSZ=0` (32-Byte-Kontexte), `ERSTMax=0` (genau **ein** ERST-Eintrag, passt zur Spec), xECP bei `+0x20`, `DBOFF=0x2000`, `RTSOFF=0x1000`. Beim Einsprung ist der Controller angehalten (`USBCMD=0`, `USBSTS=0x9`: `HCH` und `EINT`).
**Quirk:** ein 16-bit-Lesen bei `cap+2` (HCIVERSION) liefert in QEMU 8.2.2 `0`; das 32-bit-Lesen bei `cap+0` liefert `0x01000040`. Der Treiber soll Capability-Register nur mit 32-bit-Zugriffen lesen. Protokolle: **Ports 1 bis 4 USB 3.0, Ports 5 bis 8 USB 2.0** (`-device qemu-xhci` Standard `p2=4,p3=4`).
Nach `HCRST` und No-Op (Command-Ring, Event-Ring mit einem Segment, `RS=1`): `EVENT trb type=33 ... compl code=1` (Command Completion, `SUCCESS`), Zeiger auf das No-Op-TRB.

### 4. An welchem Root-Port und mit welcher Geschwindigkeit hängen `usb-storage` und `usb-kbd`?
`-device usb-storage,bus=xhci.0,... -device usb-kbd,bus=xhci.0,id=kbd0`; `PORTSC` beim Einsprung (Firmware hat die Ports schon freigegeben):
```
PORTSC[1] after-boot = 0x00001203: CCS=1 PED=1 PP=1 PLS=0 SPEED=4     <- usb-storage (USB3, SuperSpeed)
PORTSC[2..5], [7], [8] = 0x000002a0: CCS=0 PED=0 PP=1 PLS=5 (RxDetect)
PORTSC[6] after-boot = 0x00000e03: CCS=1 PED=1 PP=1 PLS=0 SPEED=3     <- usb-kbd (USB2, High-Speed)
(HMP) info usb:
  Device 0.1, Port 1, Speed 5000 Mb/s, Product QEMU USB MSD, ID: stick0
  Device 0.2, Port 2, Speed 480 Mb/s, Product QEMU USB Keyboard, ID: kbd0
```
`usb-storage` liegt am USB3-Root-Port **1** (`PORTSC.PortSpeed` 4 = SuperSpeed), `usb-kbd` am USB2-Root-Port **6** (HMP-Port 2 der USB2-Hälfte, `PortSpeed` 3 = High-Speed). Die Zuordnung folgt der Reihenfolge der Geräte und dem Protokoll; ohne `usb-kbd` ist Port 6 leer. Nach unserem eigenen `HCRST` bleibt Port 1 freigegeben (`0x00021203`: `PED=1`, `PLS=0`, `CSC=1`), Port 6 steht auf Polling ohne Freigabe (`0x00020ee1`: `PED=0`, `PLS=7`, `CSC=1`). Das bestätigt Spec §7.1 Schritt 5: USB3-Ports sind nach dem Link-Training frei, USB2-Ports brauchen einen Port-Reset (und `CSC` ist nach `HCRST` für alle Ports gesetzt, aber ohne Event).
EP0-Max-Packet für Task 20: Stick SuperSpeed 512, Tastatur High-Speed 64. **[nicht geprüft]** Vendor-/Product-IDs (kein Deskriptor gelesen; Task 20 liest sie und trägt sie in `device_descriptor_vendor_matches_qemu` ein).

### 5. Erreicht `send-key` bei vorhandenem `usb-kbd` die USB-Tastatur oder PS/2?
Die USB-Tastatur. Der Kernel reicht PS/2-Scancodes aus der bestehenden Queue durch (`PS2 scancode ...`); die USB-Tastatur wird im Wegwerf-Kernel nicht bedient, ihre HID-Warteschlange in QEMU fasst 16 Ereignisse.
```
Kontrolle ohne usb-kbd:  send-key u ; HMP "sendkey u"  -> 4 PS2-Zeilen im Serial-Log (0x16, 0x96, 0x16, 0x96)
Mit usb-kbd:             dieselben zwei Befehle        -> 0 PS2-Zeilen
Mit usb-kbd, 12 x send-key u (24 Ereignisse), -trace hid_kbd_queue_full -trace input_event_key_qcode:
                         PS2-Zeilen: 0; input_event_key_qcode: 24; hid_kbd_queue_full: 8   (24 - 16 = 8)
Mit usb-kbd, device_del kbd0, danach send-key u:  PS2 scancode 0x16 / 0x96 (wieder PS/2)
```
Weitere Befunde: QMP `input-send-event` mit `device: "kbd0"` bricht QEMU 8.2.2 ab (`Property 'qemu-fixed-text-console.device' not found`), also nicht verwenden. HMP `sendkey` nimmt denselben Weg wie `send-key`. Die USB-Tastatur puffert höchstens 16 Ereignisse; Tasten, die vor dem Start des HID-Endpunkts gesendet werden, gehen darüber hinaus verloren.
Das Boot-Protokoll-Ende zu Ende (USB-Tastatur tippt in die Shell) bleibt bis Task 22 offen: bis zur QEMU-Warteschlange ist belegt, dass `send-key` die USB-Tastatur adressiert. Die Abspeckregel §7.3 ("`send-key` erreicht `usb-kbd` nicht") greift **nicht**.

### 6. Erzeugt `device_del` eine Port-Status-Change mit `CCS=0`?
Ja. Controller mit Event-Ring und `RS=1`, dann QMP `device_del stick0` und später `device_del kbd0`:
```
QMP device_del {"id":"stick0"} -> {"return": {}}
EVENT "DEVICE_DELETED" {"device": "stick0", ...}
EVENT trb type=34 d0=0x1000000 d2=0x1000000 d3=0x8801 (compl code=1 port_id=1)
PORTSC[1] changed 0x00021203 -> 0x000202a0: CCS=0 SPEED=0 CSC=1
QMP device_del {"id":"kbd0"} -> {"return": {}}
EVENT trb type=34 d0=0x6000000 d2=0x1000000 d3=0x8801 (compl code=1 port_id=6)
PORTSC[6] changed 0x00020ee1 -> 0x000202a0: CCS=0 SPEED=0 CSC=1
```
Port Status Change Event (Typ 34, Port-ID in `d0[31:24]`) mit Completion-Code 1; `PORTSC` danach `0x000202a0`: `CCS=0`, `PED=0`, `PP=1`, `PLS=5` (RxDetect), `CSC=1`. Das Abziehen braucht keine Mitwirkung des Gastes (anders als PCI-Hotplug); `DEVICE_DELETED` kommt ohne Zutun. `PORTSC` ändert sich im selben Poll-Durchlauf (der Kernel las beides). **[nicht geprüft]**: Latenz in Millisekunden zwischen QMP und Event, Verhalten bei `USBSTS` = `0xFFFFFFFF` (`ControllerLost` lässt sich in QEMU nicht auslösen; der Plan testet es per `simulate_read_all_ones` in Task 23).

### 7. `-action reboot=shutdown,shutdown=pause`, `SHUTDOWN`-Ereignis, `dump-guest-memory`, `screendump`
Ja, alles funktioniert (Befehlszeile wie in Frage 2, ohne `usb-kbd`, Taste `p`):
```
QMP send-key {"keys":[{"type":"qcode","data":"p"}]} -> {"return": {}}
EVENT "SHUTDOWN" {"guest": true, "reason": "guest-shutdown"}
EVENT "STOP" null
QMP query-status -> {"return": {"status": "shutdown", "singlestep": false, "running": false}}
QMP dump-guest-memory {"paging": false, "protocol": "file:dump1.elf", "format": "elf"} -> {"return": {}}
EVENT "DUMP_COMPLETED" {"result": {"total": 289341440, "status": "completed", "completed": 289341440}}
QMP screendump {"filename": "shot1.ppm"} -> {"return": {}}
```
- `reason`: Poweroff über `PM1a_CNT` ergibt `guest-shutdown`; `0xCF9 <- 6` und Triple-Fault (Null-IDT + `int3`) ergeben je `"reason": "guest-reset"` (jeweils `SHUTDOWN` und danach `status: shutdown`, weil `reboot=shutdown`). `poweroff_not_reset` (Task 11) kann also am `reason` hängen.
- `pmemsave` und `screendump` funktionieren auch im pausierten Zustand (`status: shutdown`).
- Abbild: ELF64, `e_type` 4 (Core), `e_machine` 62; **1 `PT_NOTE` + 8 `PT_LOAD`** bei 256 MiB. `PT_LOAD` ist je **Gast-Physik-Abschnitt**, nicht nur je RAM-Block; `filesz == memsz`, `paddr == vaddr`:
```
256 MiB:  paddr 0x0 (0xa0000) | 0xa0000 (0x10000) | 0xc0000 (0x4000) | 0xc4000 (0x1c000) | 0xe0000 (0x20000)
          | 0x100000 (0xff00000 = RAM) | 0x80000000 (0x1000000 = VGA-VRAM) | 0xffc00000 (0x400000 = OVMF-Flash)
4 GiB:    wie oben, aber RAM 0x100000 (0xbff00000), VGA-VRAM bei 0xc0000000, zusätzlich 0x100000000 (0x40000000)
```
  Gesamtgröße 289 341 440 Byte für `-m 256M` (RAM plus VRAM plus Flash), 4 315 873 280 Byte für `-m 4G`. Das Auswerten muss über `p_paddr` laufen und nur die `Usable`-Bereiche prüfen (Legacy- und Flash-Abschnitte sind nicht RAM). Datei hat Modus `0400`.
- Prüfung der Zuordnung: Sentinel (32 Byte im `.rodata` des Wegwerf-Kernels) bei `phys 0xd9255d8` liegt im Abbild bei Dateioffset `0xd915bc0`, einziger Treffer im ganzen Abbild; die per `pmemsave` gelesene DSDT ist byteweise gleich dem Abbildinhalt bei `0xfb7b000`.
- Dauer (TCG, 4 Kerne): Abbild 256 MiB etwa 0,3 s, 4 GiB etwa 23,7 s (`SHUTDOWN` bei 2,0 s, `DUMP_COMPLETED` bei 25,7 s). Das `SHUTDOWN`-Ereignis kam rund 1 ms nach dem QMP-`send-key` (QMP-Zeitstempel des Treibers; Taste, Kernel und S5-Schreiben eingeschlossen). Für Task 11/24 mit 4 GiB: Platz (4,3 GB) und Timeout einplanen.
- `screendump` nach dem Poweroff: 1024x768 PPM (`P6`, 255), alle 786 432 Pixel nicht schwarz (Konsolenhintergrund ist ein dunkles Blaugrau, Text hell). `screen_black_after_wipe` ist also aussagekräftig: ein unbehandelter Bildschirm ist **nicht** schwarz.

### 8. `_S5`-Wert im QEMU-DSDT (i440fx)
```
ACPI: RSDP phys=0xfb7e014 rev=2 xsdt=0xfb7d0e8 rsdt=0xfb7d074
ACPI: XSDT phys=0xfb7d0e8 len=76   (FACP 0xfb7a000 len=116, APIC, HPET, WAET, BGRT)
ACPI: FADT rev=1 DSDT=0xfb7b000 X_DSDT=0xfb7b000 SMI_CMD=0xb2 ACPI_ENABLE=0xf1 PM1a_CNT=0xb004 PM1b_CNT=0x0 flags=0x80a5
ACPI: DSDT phys=0xfb7b000 len=6860
ACPI: PM1a_CNT read = 0x1 (SCI_EN=1)
ACPI: _S5_ at dsdt+0xba3 following bytes: [12, 06, 04, 00, 00, 00, 00, 10, 3b, 5c, 2e, 5f]
ACPI: SLP_TYPa=0 SLP_TYPb=0
```
Gegenprobe über `pmemsave` (QMP `pmemsave val=0xfb7b000 size=6860`): `_S5_` bei Offset 2979, Bytes `5f 53 35 5f 12 06 04 00 00 00 00 10`, also `PackageOp(0x12) PkgLength 6, 4 Elemente, je ZeroOp`. **`SLP_TYPa = SLP_TYPb = 0`** (Spec §8.5 "i440fx `_S5` = 0" bestätigt). Der Poweroff-Wert ist `0 << 10 | 1 << 13 = 0x2000`; `outw(0xB004, 0x2000)` hat QEMU beendet (`SHUTDOWN`, `guest-shutdown`). `SCI_EN` ist bei Kernel-Einsprung schon 1 (OVMF hat ACPI aktiviert), das `ACPI_ENABLE`-Schreiben auf `SMI_CMD` (`0xB2`, Wert `0xF1`) ist hier nicht nötig, der Zweig wird in QEMU nie ausgeführt (nicht getestet).
**Wichtig:** `FACP` hat `length = 116`, `revision = 1` (OEM `BOCHS`). `X_DSDT` (Offset 140) und `X_PM1a_CNT_BLK` (Offset 172) liegen **außerhalb** der Tabelle; Bytes ab Offset 116 sind Füllbytes `0xaf` bzw. Reste (`X_DSDT` las zufällig `0xfb7b000`). Der Parser muss `X_*` nur lesen, wenn `length` sie abdeckt, sonst `DSDT` (Offset 40) und `PM1a_CNT_BLK` (Offset 64) nehmen. Dasselbe Ergebnis mit Split-OVMF (gleiche Zeilen).

### 9. Speicherart der Frames von `.bss`, Boot-Kernelstack und Bootloader-Seitentabellen
Wegwerf-Kernel, `-m 256M`; Übersetzung über die Kernel-Seitentabellen und Abgleich mit `memory_regions`:
```
kernel_addr=0xd910000 kernel_len=0xa6a1f0 kernel_image_offset=0x10000000000 stack_bottom=0x18000001000 stack_len=0x200000 ramdisk=(None, 0) rsdp=Some(0xfb7e014)
kernel ELF copy (phys 0xd910000) kind=Bootloader
.bss (16 KiB static):  pages: Usable=0 Bootloader=5      (z. B. virt 0x10000228000 -> phys 0x107000)
.data (8 KiB static):  pages: Usable=0 Bootloader=3      (z. B. virt 0x10000224000 -> phys 0xdb31000, innerhalb der ELF-Kopie)
boot kernel stack (bootloader-reported range): pages: Usable=0 Bootloader=512 UnknownUefi=0
writable PT_LOAD (.data.rel.ro/.got) 12 Seiten: Bootloader=12;  writable PT_LOAD (.data/.bss) 53 Seiten: Bootloader=53
CR3/kernel L4 phys=0x101000 kind=Bootloader
L4[32] (phys-offset mapping) -> L3 phys=0x350000 Bootloader; L3[0] -> L2 phys=0x351000 Bootloader (2-MiB-Seiten, huge=1)
L4[2],[3],[4],[5],[33] -> L3 0x102000, 0x144000, 0x34c000, 0x753000, 0x551000: alle Bootloader
L4[136] (Kernel-Heap, erst vom Kernel angelegt) -> L3 phys=0x2000 kind=Usable
```
Alle Segmente des Kernels (`.data`, `.bss`, relozierte Daten), der Boot-Kernelstack (512 Seiten = 2 MiB), die Kernel-ELF-Kopie und alle Seitentabellen des Bootloaders gehören zu `MemoryRegionKind::Bootloader`, **keine einzige Seite zu `Usable`**. Nebenbefund: die Seiten von `.data` liegen innerhalb der geladenen ELF-Kopie (der Bootloader mappt dateigestützte Seiten ohne Kopie); Nullen von `.data` verändert also die ELF-Kopie mit (unkritisch). `.bss` hat eigene Frames. Der Kernel-Heap und alle Frames, die der Kernel später selbst bekommt, kommen aus `Usable` (Stufe 7/8). Konsequenz: Stufe 9 (Kernel-Segmente und Boot-Kernelstack nullen) ist nötig, und `Usable`-Stufen 6 bis 8 decken diese Frames nicht ab. Die Behauptung der Spec §8.3 (a), Offset-Mapping-Tabellen seien nicht `Usable`, ist bestätigt (L4/L3/L2: `Bootloader`).
Speicherkarte (Auszug, `-m 256M`): `Usable` fast durchgehend ab `0x759000`, `Bootloader` `0x100000..0x759000` und `0xd910000..0xe37b000` (Kernel-ELF), `UnknownUefi` (Typen 0, 5, 6, 9, 10) für ACPI/Runtime/Reserved, und `UnknownUefi(0) 0xfd00000000..0x10000000000`. Mit `-m 4G`: `Usable 0x100000000..0x140000000` (oberhalb 4 GiB).

### 10. Deckt das Physical-Memory-Mapping des Bootloaders den MMIO-Bereich unter 4 GiB ab? (nur Info)
Ja, und mehr: `translate_addr(PHYSICAL_MEMORY_OFFSET + phys)` ist `Some(..)` für den xHCI-BAR0 (`0xC000000000`), LAPIC `0xFEE00000`, IOAPIC `0xFEC00000`, `0xC0000000` und den Framebuffer. Die höchste Region der Speicherkarte endet bei `0x10000000000` (1 TiB, wegen der `UnknownUefi(0)`-Region `0xfd00000000..0x10000000000`), also reicht das Mapping bis 1 TiB (L4-Einträge 32 und 33). PTE-Flags für die BAR-Seite: `PRESENT | WRITABLE | HUGE_PAGE | NO_EXECUTE`, **ohne** `NO_CACHE`/`WRITE_THROUGH` (cachebar, 2-MiB-Seiten). Konsequenz: das Offset-Mapping ist für MMIO-Zugriffe ungeeignet (Spec E5 mit eigenem uncached Fenster ist richtig), und der xHCI-BAR liegt **oberhalb 4 GiB**, nicht darunter. Das eigene Fenster wurde im Spike getestet: 4 KiB-Seiten mit `PRESENT|WRITABLE|NO_CACHE|NO_EXECUTE` bei `0x5000_0000_0000`, vier Seiten (16 KiB) per `map_to` mit dem Frame-Allocator des Kernels, alle Registerzugriffe darüber liefen korrekt. Der Mapping-Umfang hängt vom Speicherplan ab, den OVMF der Karte mitgibt (hier die 1-TiB-Region); eine feste Annahme "bis 4 GiB" oder "bis 1 TiB" darf der Kernel nicht treffen.

## Folgen für die Tasks 11 und 19 bis 24

**IDE-Rückfall (Spec §7.3 "Spike zeigt, dass OVMF nicht von `usb-storage` bootet"): nicht nötig.** USB-Boot ist unter QEMU belegt (Frage 2); die Abnahme läuft mit `--boot usb` (Standard) und "Boot vom USB-Stick" kann als unter QEMU belegt gelten (echte Hardware bleibt M5). Die Abspeckregel für `send-key` (Frage 5) greift ebenfalls nicht.

- **Task 11 (hotkey-Abnahme, ohne USB):**
  - Taste per `send-key` erreicht PS/2, solange kein `usb-kbd` am Gerät hängt: Das Skript darf **kein** `usb-kbd` anschließen (Frage 5).
  - `-action reboot=shutdown,shutdown=pause` ist tauglich. `SHUTDOWN.reason` unterscheidet `guest-shutdown` (Poweroff) und `guest-reset` (auch `0xCF9` und Triple-Fault), also kann `poweroff_not_reset` am `reason` hängen (Frage 7).
  - Dump: `dump-guest-memory` mit `paging=false`, `format=elf` (Protokoll `file:`), nach `STOP`/`status: shutdown`. Nach `p_paddr` auswerten, nur `Usable`-Bereiche prüfen; die Datei enthält auch VGA-VRAM, Flash und Legacy-Fenster. Dumps sind 289 MB (256 MiB) bzw. 4,3 GB (4 GiB), Modus `0400`, 4 GiB dauern etwa 24 s unter TCG.
  - `screen_black_after_wipe`: ein nicht gewipter Schirm hat den dunklen Hintergrund (alle Pixel nicht schwarz), der Test ist also aussagekräftig; `screendump` geht im pausierten Zustand.
  - Mit `-bios` ist `OVMF.fd` (Ubuntu `/usr/share/qemu/OVMF.fd`) benutzt worden; das Split-Paar bootet ebenfalls.
  - CI-Schritt: `qemu-system-x86_64 --version` mitloggen (Frage 1, nicht in CI verifiziert).
- **Task 19 (xHCI-Init):** BAR0 ist 64-bit bei `0xC000000000` oberhalb 4 GiB, 16 KiB; `memory`-Mapping-Fenster in `Bar`/`mmio` (Task 18) muss 64-bit-Basis tragen und uncached mappen. Capability-Register nur mit 32-bit-Zugriffen lesen (16-bit-Lesen von `HCIVERSION` liefert 0). `MaxSlots` 64, `MaxIntrs` 16, `MaxPorts` 8, `AC64=1`, `CSZ=0` (32-Byte-Kontext), `MaxScratchpadBufs=0` (Scratchpad-Zweig ungetestet), `ERSTMax=0` (ein ERST-Eintrag genügt und ist das Maximum). Controller ist beim Einsprung angehalten (`HCH`), PCI-Command `0x7`, Reset (`HCRST`, `CNR`) und No-Op (Event-Typ 33, Code 1) laufen sauber, `command_ring_wraps` nutzt 64 Plätze mit Link-TRB (im Spike nur ein No-Op getestet).
- **Task 20 (Enumeration):** Stick an Root-Port 1 (USB3, `PortSpeed` 4, EP0 512), Tastatur an Root-Port 6 (USB2, `PortSpeed` 3, EP0 64); Ports 1 bis 4 USB3, 5 bis 8 USB2. `enumerates_keyboard_and_storage` kann diese Ports als Erwartung benutzen (abhängig von der Reihenfolge der Geräte an der Kommandozeile; die Tests sollen das Gerät nach Klasse, nicht nur nach Port prüfen). `port_ack_keeps_port_enabled`: Port 1 bleibt nach `HCRST` freigegeben, Port 6 braucht Port-Reset (`PED=0`, `PLS=7` nach `HCRST`). Nach dem Reset ist `CSC=1` an **allen** Ports gesetzt (auch leeren): quittieren, ohne `PED` zu schreiben. Vendor/Product-IDs sind **nicht** gemessen (Task 20 liest sie).
- **Task 21 (Stick erkennen, versiegeln):** Boot vom Stick klappt; der Stick ist beim Kernel-Einsprung am Port 1 sichtbar (`CCS=1`). `acpi::finds_s5_in_qemu_tables`: `SLP_TYPa=SLP_TYPb=0`, `PM1a_CNT_BLK=0xB004`, `PM1b_CNT_BLK=0`, `SMI_CMD=0xB2`/`ACPI_ENABLE=0xF1`, `SCI_EN` schon 1. **FADT hat Revision 1, 116 Byte:** der Parser darf `X_DSDT`/`X_PM1a_CNT_BLK` nicht blind lesen, sondern prüft die Länge (Spec §8.5 anpassen, Tests für `Fadt` in Task 6 um "FADT ohne Extended-Felder" ergänzen). Der Boot-Marker-Test ist unabhängig von der Boot-Art.
- **Task 22 (USB-Tastatur):** `send-key` und HMP `sendkey` gehen bei `usb-kbd` an die USB-Tastatur (nicht PS/2), bis zu 16 Ereignisse werden in QEMU gepuffert, danach gehen sie verloren (`hid_kbd_queue_full`); das Skript soll mit dem Tippen warten, bis der HID-Endpunkt läuft (`security` zeigt Berichte > 0). `keyboard_unplug_does_not_wipe` funktioniert wie geplant: nach `device_del kbd0` geht `send-key` an PS/2 (Frage 5). `input-send-event` mit `device` nicht verwenden (QEMU bricht ab). Tastatur-Abziehen erzeugt ein Typ-34-Event an Port 6 (Frage 6), das nur als "Tastatur weg" gilt.
- **Task 23 (Abzieh-Auslöser):** Event Typ 34, Port-ID in `d0[31:24]`, Completion-Code 1, danach `PORTSC` `0x000202a0` (`CCS=0`, `CSC=1`, `PED=0`, `PLS=5`): `classify` soll genau diese Form als `Disconnected` erkennen, ein Wert wie `0x00021203` (`CCS=1`, `CSC=1` nach Reset) ist kein Abziehen. Beide Wege (Event und `PORTSC`-Lesen) liefern das Ergebnis. `ControllerLost` ist in QEMU nicht auslösbar (Test bleibt `simulate_read_all_ones`; **nicht geprüft**).
- **Task 24 (Abzieh-Abnahme, Messung):** `--boot usb` ist der Standard und genügt; `--boot ide` bleibt Option (nicht nötig; `ide-hd` braucht `snapshot=on`, `usb-storage` nimmt `readonly=on`). `device_del stick0` gibt sofort `{"return": {}}` und `DEVICE_DELETED`, das Port-Event kommt ohne Gastmitwirkung. Messung: TCG ohne KVM, 4 Kerne, Dump-Dauer 4 GiB etwa 24 s (Timeouts 120 s einplanen), `SHUTDOWN` kam rund 1 ms nach `send-key`. Poweroff-Wert `0x2000` auf Port `0xB004` (nicht `0xB000`).
