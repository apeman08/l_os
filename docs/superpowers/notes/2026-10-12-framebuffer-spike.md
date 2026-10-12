# Framebuffer-Spike und Bootloader-Entscheidung (Task 1, M1 "Aurora")

Datum: 2026-10-12. Ergebnis von Task 1 des Plans `docs/superpowers/plans/2026-10-12-zukunftsplan.md`.
Spike-Code lag ausschließlich im Scratchpad (`fb-spike-THROWAWAY/`, außerhalb des Repos) und ist
nicht Teil dieses Commits. Alles, was nicht tatsächlich ausgeführt wurde, ist mit **[assumed]** markiert.

Umgebung: `qemu-system-x86_64` 8.2.2, `bootimage` 0.10.5, `nightly-2026-10-11` (aus `rust-toolchain.toml`),
OVMF aus dem Ubuntu-Paket `ovmf` (`/usr/share/qemu/OVMF.fd`).

## Kurzfassung

- `bootloader` 0.9 kann keinen hochauflösenden Framebuffer liefern (Quellcode geprüft).
- `bootloader` 0.11.17 **im BIOS-Modus** liefert einen Framebuffer, erfüllt aber die Anforderung
  ≥ 1024×768 bei 32 bpp **nicht**: Stage 2 begrenzt fest auf maximal 1280×720, QEMU wählt dabei 24 bpp.
- `bootloader` 0.11.17 **im UEFI-Modus (OVMF)** erfüllt die Anforderung deterministisch:
  1024×768, 4 Byte/Pixel, BGR, `stride` 1024, belegt per `screendump`.
- Tests mit `isa-debug-exit` + Serial + `custom_test_frameworks` laufen ohne `bootimage` über einen eigenen
  Cargo-`runner` (im Spike ausgeführt, Erfolg und Fehlschlag geprüft).
- **Empfehlung: (a)** Migration auf `bootloader` 0.11 mit eigenem Image-Builder-Crate, in QEMU per UEFI booten.
  (b) eigene VBE-Stufe wird verworfen.

## 1. Was `bootloader` 0.9 kann

Geprüft am Quellcode von `bootloader-0.9.35` (Version aus `Cargo.lock`, Registry-Quelle unter `~/.cargo/registry/src/`):

- Features in `Cargo.toml`: `binary`, `map_physical_memory`, `recursive_page_table`, `sse`,
  `vga_320x200 = ["font8x8"]`. Es gibt kein VBE/VESA- oder Framebuffer-Feature.
- `src/main.rs` bindet genau eine von zwei Videomodus-Dateien ein (`#[cfg(feature = "vga_320x200")]`):
  - `src/video_mode/vga_text_80x25.s`: `int 0x10` mit `ax = 0x0003` (80×25 Text, 16 Farben), Ausgabe nach `0xb8000`.
  - `src/video_mode/vga_320x200.s`: `int 0x10` mit `ax = 0x0013` (320×200, 256 Farben, Palette), Ausgabe nach `0xa0000`.
- `vga_map_frame_buffer` (in beiden `.s`-Dateien) bildet nur `0xa0000..0xc0000` identisch ab; `stage_3.s` ruft es als `map_framebuffer` auf.
- `BootInfo` (`src/bootinfo/mod.rs`) enthält `memory_map`, optional `physical_memory_offset` und
  `recursive_page_table_addr`, aber **keine Framebuffer-Information**.
- Kein Code in 0.9 ruft VBE-Funktionen (`int 0x10`, `ax = 0x4F0x`) auf (Suche nach `vesa`/`vbe`/`framebuffer`/`0x4f0`: nur der Label-Name `map_framebuffer`; `int 0x10` kommt nur in den beiden Videomodus-Dateien und in `stage_1.s` (Teletype-Ausgabe `ah = 0x0e`) vor).

Damit ist die Annahme aus dem Plan belegt: mit 0.9 sind nur VGA-Text und 320×200×8 möglich. Ein eigener
Modus wäre nur über einen Fork des Real-Mode-Teils (`config_video_mode`) möglich.

## 2. Nachweis mit `bootloader` 0.11.17

### Spike-Aufbau (Wegwerf-Code, nicht im Repo)

- `kernel/`: `bootloader_api = "0.11.17"`, `x86_64 = "0.15.5"`, gebaut mit **dem unveränderten
  `x86_64-l_os.json`** des Repos (`-Zjson-target-spec`, `-Zbuild-std=core,compiler_builtins`). Der Kernel
  schreibt einen Verlauf (R wächst nach rechts, G nach unten, B = 0x40) plus ein weißes 64×64-Quadrat oben
  links, beachtet `pixel_format`, `stride` und `bytes_per_pixel`, gibt `FrameBufferInfo` auf COM1 aus und
  beendet QEMU über `isa-debug-exit` (Port `0xf4`, Wert `0x10` → Exit-Code 33).
- `builder/`: Host-Crate mit `bootloader = "0.11.17"` (Features `bios`, `uefi`), ruft
  `BiosBoot::new(..).create_disk_image(..)` bzw. `UefiBoot::new(..)` auf.
- QEMU mit `-display none`, Monitor über Unix-Socket, `screendump <datei>.ppm`; die PPM wurde mit
  einem kleinen Python-Skript ausgewertet (Header, Eckpixel, SHA-256).

### Ergebnisse (alle ausgeführt)

| Variante | QEMU-Argumente | `FrameBufferInfo` (Serial) | PPM |
|---|---|---|---|
| BIOS, Mindestgröße 1024×768 gesetzt | (Standard-VGA) | 1280×720, `Bgr`, 3 Byte/Pixel, `stride` 1280, `byte_len` 2764800 | P6 1280×720 |
| UEFI, Mindestgröße 1024×768 gesetzt | `-bios OVMF.fd` | 2560×1600, `Bgr`, 4 Byte/Pixel, `stride` 2560 | P6 2560×1600 |
| UEFI, Mindestgröße gesetzt | `-bios OVMF.fd -vga none -device VGA,vgamem_mb=4` | 1360×768, `Bgr`, 4, `stride` 1360 | P6 1360×768 |
| UEFI, keine Mindestgröße | `-bios OVMF.fd` | 1280×800, `Bgr`, 4, `stride` 1280 | P6 1280×800 |
| **UEFI, keine Mindestgröße** | **`-bios OVMF.fd -vga none -device VGA,xres=1024,yres=768`** | **1024×768, `Bgr`, 4 Byte/Pixel, `stride` 1024, `byte_len` 3145728** | **P6 1024×768** |

PPM-Auswertung der empfohlenen Variante (`screendump` liefert RGB, unabhängig vom BGR-Speicherformat):

- Pixel (10,10) = (255,255,255) → Markierung oben links, Orientierung stimmt.
- oben rechts (255,0,64), unten links (0,255,64), unten rechts (255,255,64), Mitte (127,127,64)
  → Kanäle richtig zugeordnet, also ist `Bgr` korrekt gemeldet und korrekt beschrieben.
- Zwei unabhängige Läufe ergaben dieselbe SHA-256 (`d18907b71da21a5a…`) → Screendump ist deterministisch,
  Pixel-Hash-Tests (Spec, Abschnitt Tests) sind machbar.

Die BIOS-Variante zeigte ebenfalls einen korrekten Verlauf (gleiche Eckwerte), nur in 1280×720×24.

### Warum BIOS nicht reicht (Quellcode geprüft)

- `bootloader-x86_64-bios-stage-2-0.11.17/src/main.rs`: `let max_width = 1280; let max_height = 720;`
  mit dem Kommentar `// TODO: load these from the kernel's config instead of hardcoding`.
- `src/vesa.rs`, `get_best_mode`: verwirft Modi über dem Maximum und nimmt den breitesten/höchsten;
  bei gleicher Größe wird ein Modus mit bekanntem Pixelformat nicht ersetzt, die Farbtiefe wird nicht
  bevorzugt. Ergebnis in QEMU: 1280×720 mit 24 bpp.
- `minimum_framebuffer_width/height` wird in BIOS-Stage 4 nur in die Konfiguration kopiert; die Wahl
  des Modus ist da schon in Stage 2 gefallen. Im BIOS-Pfad wirkt die Einstellung also nicht.
- Im Kernel-Config (`BootloaderConfig::frame_buffer`) ist das Feld seit 0.11 als veraltet markiert
  (Compiler-Warnung: Konfiguration über `BootConfig` beim Erzeugen des Images).

1024×768 ist im BIOS-Pfad also gar nicht erreichbar (768 > 720). Nur mit einem Fork von Stage 2 ginge das.

### Warum UEFI so gewählt wird (Quellcode geprüft)

`bootloader-x86_64-uefi-0.11.17/src/main.rs`, `init_logger`: Ist eine Mindestgröße gesetzt, wird der
**letzte** GOP-Modus genommen, der sie erfüllt (in OVMF der größte, z. B. 2560×1600). Ohne Mindestgröße
bleibt der aktuelle GOP-Modus; den bestimmt QEMU über `-device VGA,xres=1024,yres=768` (bevorzugter
EDID-Modus). Bei UEFI ist `bytes_per_pixel` fest 4; `Bitmask`/`BltOnly` führen zu einem Panic.

Konsequenz: **keine** Mindestgröße konfigurieren, Auflösung über die QEMU-Argumente festlegen.
`stride` ist in Pixeln angegeben (nicht in Bytes). Der Renderer soll trotzdem `stride`, `bytes_per_pixel`
(3 oder 4) und `Rgb`/`Bgr` aus `FrameBufferInfo` beachten und nichts fest annehmen.

Weitere Beobachtung: 0.11 legt `physical_memory_offset` bei `Mapping::Dynamic` auf `0x200_0000_0000`
(Serial-Ausgabe 2199023255552).

## 3. Tests, `bootimage`-Ersatz, Smoke-Test und CI

### Tests ohne `bootimage` (im Spike ausgeführt)

Der Spike-Kernel hat `#![feature(custom_test_frameworks)]`, `#![test_runner(..)]`,
`#![reexport_test_harness_main = "test_main"]` und einen `#[test_case]`. In `.cargo/config.toml`
ersetzt ein eigenes Skript den `bootimage runner`:

```
[target.'cfg(target_os = "none")']
runner = "<pfad>/runner.sh"
```

Der Runner erhält von Cargo den Pfad der Test-ELF, baut daraus mit dem Builder ein UEFI-Image und startet
`qemu-system-x86_64 -drive format=raw,file=<img> -bios OVMF.fd -cpu max -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial stdio -display none -no-reboot -vga none -device VGA,xres=1024,yres=768`
unter `timeout 300`. Exit-Code 33 wird auf 0 abgebildet, alles andere auf 1.

- Ausgeführt: `cargo test` → Serial zeigt `running 1 tests`, `trivial... ok`, QEMU-Exit 33, `cargo test` Exit 0.
- Gegenprobe: `assert_eq!(1 + 1, 3)` → Panic-Handler schreibt `0x11`, QEMU-Exit 35, `cargo test` Exit 1.
- Auch das BIOS-Image beendet sich mit `isa-debug-exit` korrekt mit 33 (ausgeführt).
- Laufzeit pro Boot bis zum Exit: etwa 4,9 s (BIOS und UEFI gemessen, Spike-Kernel, `-cpu max`).

Stolperstein im Spike: `[profile.*] panic = "abort"` im Kernel-`Cargo.toml` führte bei `cargo test` zu
`duplicate lang item ... sized` (zwei `core`-Builds). Das Repo setzt `panic-strategy` bereits im Target-JSON
und hat keine solchen Profile, also wie bisher belassen.

Was das l_os-Paket für Task 7 übernehmen muss (aus `Cargo.toml` abgeleitet):

- `test-args`, `test-success-exit-code = 33` und `test-timeout = 300` aus `[package.metadata.bootimage]`
  wandern in den Runner (inkl. `-netdev ... -object filter-dump,...,file=target/net-trace.pcap`, die
  `scripts/net_tests.sh` braucht).
- `run-args` wandern in ein Run-Kommando (Runner-Modus ohne Test, erkennbar am Pfad unter `target/.../deps/` [assumed]).
- `harness = false`-Tests (`should_panic`, `exception_*`, ...) laufen genauso über den Runner [assumed, nicht einzeln geprüft].

### Image-Builder-Crate

- Eigenes Host-Crate (z. B. `tools/image`), das von `bootloader = "0.11"` abhängt und
  `UefiBoot`/`BiosBoot` aufruft. Es darf nicht unter dem `[build] target = "x86_64-l_os.json"` der
  Repo-`.cargo/config.toml` gebaut werden: eigenes Verzeichnis mit eigener Konfiguration oder explizites
  `--target` des Hosts [assumed, Detail für Task 7].
- Erstes Bauen dauert etwa 60 s pro Feature-Satz, weil `bootloader`'s `build.rs` die Stages per
  `cargo install bootloader-x86_64-bios-*/uefi --version 0.11.17 -Zbuild-std=core` von crates.io baut
  (gemessen: 59 s für `bios`, 64 s zusätzlich für `uefi`). Das braucht Netzwerk und Nightly; mit dem
  gepinnten Nightly hat es funktioniert.
- `cargo bootimage` entfällt. Ersatz: ein Befehl, der den Kernel baut und das Image unter einem festen
  Pfad ablegt (z. B. `target/x86_64-l_os/debug/l_os-uefi.img`).

### Smoke-Test (`scripts/qemu_shell_smoke.py`, `scripts/qemu_fetch_smoke.py`)

- Beide lesen `target/x86_64-l_os/debug/bootimage-l_os.bin`; der Pfad ändert sich.
- `qemu_shell_smoke.py` liest den VGA-Textpuffer mit `pmemsave 0xb8000 4000`. Bei UEFI gibt es keinen
  VGA-Textmodus, und auch der 0.11-BIOS-Pfad schaltet in einen VESA-Grafikmodus. Damit funktioniert
  dieser Weg nicht mehr [assumed, aus dem Videomodus abgeleitet, nicht mit l_os ausgeführt].
  Vorschlag: Bildschirmtext zusätzlich auf Serial spiegeln und darauf prüfen; die Optik prüfen
  Pixel-Hash-Screendumps (der Spike zeigt, dass sie deterministisch sind).
- QEMU-Aufruf ergänzen: `-bios /usr/share/qemu/OVMF.fd -vga none -device VGA,xres=1024,yres=768`.
  UEFI startet erst nach dem OVMF-Boot; die feste Wartezeit von 3 s im Skript kann zu knapp sein
  (Spike-Exit nach ~4,9 s) [assumed].

### CI (`.github/workflows/ci.yml`)

- `cargo install bootimage --locked --version 0.10.5` entfällt.
- `apt-get install` zusätzlich `ovmf`. Der Pfad der Firmware im Runner (`/usr/share/qemu/OVMF.fd` hier,
  auf `ubuntu-24.04` evtl. `/usr/share/ovmf/OVMF.fd`) ist im Runner-Skript konfigurierbar zu machen [assumed].
- „Smoke test“-Schritt: `cargo bootimage` durch den Image-Befehl ersetzen.
- Der erste Build braucht crates.io für die Bootloader-Stages; Cargo-Cache empfohlen [assumed].
- README (Abschnitt Requires/Run) anpassen.

## 4. Aufwand und Risiken

Aufwand für Task 7 (Schätzung, [assumed]): **2–3 Personentage**.

- Kernel-Port auf `bootloader_api`: `entry_point!` mit `&'static mut BootInfo`, `memory_map` →
  `memory_regions` (andere Typen), `physical_memory_offset` ist `Option`. Betroffen: `src/main.rs`,
  `src/lib.rs`, `src/memory.rs` und 11 Testdateien (20 Fundstellen von `bootloader::`).
- Runner + Image-Builder-Crate, Smoke-Skripte, CI, README.
- `src/vga_buffer.rs` (inkl. `emergency_print`) durch Framebuffer-/Serial-Ausgabe ersetzen; das ist
  ohnehin `src/fb_console.rs` im Plan.

Risiken:

- **Feste virtuelle Adressen** (`HEAP_START = 0x4444_4444_0000`, `USER_CODE_ADDR = 0x7000_0000_0000`):
  0.11 platziert Kernel, Stack, BootInfo und Physical-Mapping dynamisch. Kollisionen sind möglich
  [assumed]; Abhilfe: `Mapping::FixedAddress` in `BootloaderConfig`.
- **0.9-spezifische Annahmen** im Speichermanagement (Frame-Allocator über `MemoryMap`, Seitentabellen-Layout) [assumed].
- **Kein VGA-Text mehr**: Panic-Ausgabe und Smoke-Test brauchen Ersatz, bevor der Framebuffer-Renderer steht.
- **Netzwerkabhängiger Build** der Bootloader-Stages (Proxy, CI-Cache).
- **OVMF-Verhalten**: Auflösung hängt an `-device VGA,xres/yres` und dem GOP-Treiber; auf echter
  Hardware (M5) bestimmt die Firmware den Modus. Der Renderer muss beliebige Größen ab 1024×768 vertragen.
- **Bootzeit**: UEFI-Boot verlängert jede der 23 QEMU-Testdateien unter `tests/` etwas (absolute Zahlen für l_os nicht gemessen) [assumed].

## 5. Empfehlung

**(a) Migration auf `bootloader` 0.11.17 mit eigenem Image-Builder-Crate, Boot in QEMU per UEFI (OVMF).**

Begründung:

- Nachweislich 1024×768, 32 bpp, BGR, `stride` 1024, ohne Patch des Bootloaders.
- Tests, `isa-debug-exit` und Serial funktionieren mit einem eigenen Runner (ausgeführt).
- Die Spec verlangt in M5 UEFI-Boot auf echter Hardware. 0.9 ist BIOS-only, (b) wäre also ein Umweg,
  der in M5 doch durch die Migration ersetzt werden müsste.
- (b) hieße, den Real-Mode-Teil von 0.9 zu forken (VBE `4F01/4F02` statt `ax = 0x0013`), das LFB in
  `stage_3.s` abzubilden und `BootInfo` zu erweitern – Assembler-Wartung ohne Mehrwert über M1 hinaus.
- Der BIOS-Pfad von 0.11 bleibt als Fallback denkbar, erfüllt aber ohne Fork von Stage 2 nicht 1024×768×32
  (max. 1280×720, in QEMU 24 bpp).

Nicht untersuchte Alternative (nur zur Vollständigkeit, [assumed]): mit 0.9 bleiben und QEMUs
Bochs-VBE-Register (Ports `0x1CE/0x1CF`) aus dem Kernel programmieren. Das liefe nur in QEMU/Bochs und
löst M5 nicht; daher nicht weiterverfolgt.
