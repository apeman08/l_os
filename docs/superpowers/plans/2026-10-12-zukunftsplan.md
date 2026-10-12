# l_os Zukunftsplan Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** l_os zu einem amnesischen, ausschließlich über Tor vernetzten Terminal-System mit modernem Grafik-Terminal und Terminal-Browser ausbauen; dieser Plan setzt M1 (Grafik-Terminal "Aurora") vollständig um und legt M2–M6 als Rahmen fest.

**Architecture:** Alle Darstellungslogik (Zellenraster, Font, Theme, Widgets, Animation, Pixel-Rendering) lebt als reiner, `forbid(unsafe_code)`-Code in `crates/l_core` und ist per Host-Test prüfbar. Der Kernel stellt nur Framebuffer-Zugriff bereit (`src/fb_console.rs`) und ersetzt `src/vga_buffer.rs` hinter derselben API. Spätere Meilensteine (USB/Wipe, Tor, Browser, Hardware) bekommen je eine eigene Spec und einen eigenen Plan.

**Tech Stack:** Rust nightly-2026-10-11, `bootloader` (0.9 heute; Migration auf 0.11 für Framebuffer, siehe Task 1), QEMU, Host-Tests mit `cargo +stable` über `scripts/host_tests.sh`.

**Spec:** `docs/superpowers/specs/2026-10-12-zukunftsplan-design.md`

## Global Constraints

- Nichts darf als verifiziert beschrieben werden, was kein Test belegt (`docs/PLAN_NEXT.md`, Leitregeln).
- Test zuerst schreiben, Fehlschlag sehen, dann implementieren; ein Commit pro Aufgabe.
- Vor jedem Commit: `scripts/host_tests.sh`, `cargo test` und `python3 -I scripts/qemu_shell_smoke.py` (sobald vom Task berührt).
- Jeder neue `unsafe`-Block bekommt einen `// SAFETY:`-Kommentar; `crates/l_core` bleibt `#![forbid(unsafe_code)]`.
- Neue Abhängigkeit nur nach Prüfung von `no_std`, Allokation, `unsafe`, Wartung, Lizenz.
- Toolchain bleibt `nightly-2026-10-11` (`rust-toolchain.toml`).
- UI-Regeln aus der Spec: höchstens drei Tastenhinweise pro Bildschirm; genau ein Akzent; Animationen abschaltbar; Kontrast Text/Hintergrund ≥ 7:1, alle übrigen Textfarben ≥ 4.5:1.
- Netz nur über Tor, Daten nur im RAM, kein JavaScript im Browser (gilt ab M2–M4; in M1 nichts davon verletzen).

## Review Focus

- Zeichen ohne Glyphe (Emoji, CJK, Kombinationszeichen): sichtbare Ersatzglyphe, kein Panic, kein Layout-Versatz (Task 3).
- Text breiter als das Raster oder Zeile ohne Platz: wird am Rand abgeschnitten, schreibt nie außerhalb des Rasters (Task 2).
- Framebuffer mit BGR statt RGB, ungerader Breite oder `stride` > `width`: korrekte Farben, keine Zeilenverschiebung (Task 5).
- Lange Ausgaben (tausende Zeilen) und Tastenanschläge: Scrollen zeichnet nur geänderte Zeilen, Eingabe bleibt flüssig (Tasks 2, 7).
- Panic während die Konsole gesperrt ist: Notausgabe erscheint trotzdem (Task 7).
- Mehr als drei Tastenhinweise oder leere Statuszeile: Typfehler bzw. definiertes Verhalten, kein Layoutbruch (Task 6).

---

## Meilenstein M1 – Grafik-Terminal "Aurora"

### Dateistruktur

- `crates/l_core/src/cell.rs` – `Rgb`, `Cell`, `Grid`, `Rect`, Dirty-Tracking
- `crates/l_core/src/font.rs` – eingebetteter 8×16-Bitmap-Font, `glyph()`
- `crates/l_core/src/theme.rs` – Farb-Tokens, Kontrastberechnung
- `crates/l_core/src/render.rs` – Raster → Pixelpuffer
- `crates/l_core/src/widgets.rs` – Rahmen, Statusleiste, Hinweiszeile
- `crates/l_core/src/anim.rs` – Easing, Fade, Reduce-Motion
- `src/fb_console.rs` – Kernel-Konsole auf dem Framebuffer (ersetzt `src/vga_buffer.rs`)
- `docs/superpowers/notes/2026-10-12-framebuffer-spike.md` – Ergebnis von Task 1

### Task 1: Spike – Framebuffer-Boot

Zweck: klären, wie ein Framebuffer in QEMU entsteht, bevor irgendetwas darauf gebaut wird. `bootloader` 0.9 bietet im BIOS-Modus nur VGA-Text (und 320×200 mit 256 Farben) und ist daher für Aurora ungeeignet **[assumed, im Spike zu belegen]**.

**Files:**
- Create: `docs/superpowers/notes/2026-10-12-framebuffer-spike.md`
- Throwaway: Spike-Code in einem Wegwerf-Branch/Verzeichnis, nicht in `main`

**Interfaces:**
- Produces: Entscheidung (a) Migration auf `bootloader` 0.11 mit eigenem Image-Builder-Crate, oder (b) eigene VBE-Stufe, mit Auswirkungen auf `cargo bootimage`, `cargo test`, `scripts/qemu_shell_smoke.py` und CI. Task 7 setzt die Entscheidung um.

- [ ] **Step 1:** Prüfen, welche Framebuffer-Fähigkeiten `bootloader` 0.9 hat (Features, Doku, Quellcode) und das Ergebnis in die Notiz schreiben.
- [ ] **Step 2:** Minimalen Kernel mit `bootloader` 0.11 (BIOS) bauen, der einen Farbverlauf in einen ≥ 1024×768-32-bpp-Framebuffer schreibt; in QEMU per `screendump` (PPM) belegen. Als Wegwerf-Code kennzeichnen.
- [ ] **Step 3:** In der Notiz festhalten: Pixelformat, Auflösung, `stride`, wie Tests (`isa-debug-exit`, Serial) und `bootimage`-Ersatz funktionieren, CI-Änderungen, geschätzter Aufwand. Empfehlung (a) oder (b).
- [ ] **Step 4:** Commit der Notiz.

```bash
git add docs/superpowers/notes/2026-10-12-framebuffer-spike.md
git commit -m "docs: Framebuffer-Spike und Bootloader-Entscheidung"
```

### Task 2: Zellenraster (`cell.rs`)

**Files:**
- Create: `crates/l_core/src/cell.rs`
- Modify: `crates/l_core/src/lib.rs` (Modul eintragen)

**Interfaces:**
- Produces:
  - `pub struct Rgb(pub u8, pub u8, pub u8)`
  - `pub struct Cell { pub ch: char, pub fg: Rgb, pub bg: Rgb, pub bold: bool }`
  - `pub struct Rect { pub col: usize, pub row: usize, pub cols: usize, pub rows: usize }`
  - `Grid::new(cols: usize, rows: usize, fill: Cell) -> Grid`
  - `Grid::cols(&self) -> usize`, `Grid::rows(&self) -> usize`, `Grid::get(&self, col: usize, row: usize) -> Option<&Cell>`
  - `Grid::set(&mut self, col: usize, row: usize, cell: Cell)` (außerhalb: ignoriert)
  - `Grid::put_str(&mut self, col: usize, row: usize, s: &str, fg: Rgb, bg: Rgb) -> usize` (liefert geschriebene Spalten, schneidet am Rand ab)
  - `Grid::scroll_up(&mut self, lines: usize, fill: Cell)`
  - `Grid::take_dirty(&mut self) -> Option<core::ops::Range<usize>>` (Zeilenbereich seit letztem Aufruf)

- [ ] **Step 1: Failing tests** in `cell.rs`: `put_str_clips_at_right_edge` (Raster 10 Spalten, `put_str(7, 0, "abcdef", ..)` liefert `3`, Zelle (9,0) ist `'c'`), `set_out_of_bounds_is_ignored`, `scroll_up_moves_rows_and_fills_last`, `dirty_range_covers_only_touched_rows` (nur Zeilen 3 und 5 gesetzt → `Some(3..6)`, danach `None`), `put_str_in_row_beyond_grid_returns_zero`.
- [ ] **Step 2:** `scripts/host_tests.sh cell::` → FAIL (Modul fehlt).
- [ ] **Step 3:** Implementieren; Dirty-Tracking als Min/Max der berührten Zeilen, `Vec<Cell>` zeilenweise.
- [ ] **Step 4:** `scripts/host_tests.sh cell::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): cell grid with dirty tracking`.

### Task 3: Font (`font.rs`)

**Files:**
- Create: `crates/l_core/src/font.rs` (Glyphdaten eingebettet)
- Modify: `crates/l_core/src/lib.rs`
- Modify: `docs/ARCHITECTURE.md` (Fontquelle und Lizenz eintragen)

**Interfaces:**
- Produces: `pub const GLYPH_W: usize = 8; pub const GLYPH_H: usize = 16; pub fn glyph(ch: char) -> &'static [u8; GLYPH_H]` (Bit 7 = linkes Pixel; liefert für unbekannte Zeichen die Ersatzglyphe, niemals Panic).

- [ ] **Step 1: Font wählen:** Ein 8×16-Bitmap-Font mit kompatibler Lizenz (Kandidat: Terminus, SIL OFL); Lizenz im Repo ablegen und in `ARCHITECTURE.md` benennen. Wenn die Lizenz nicht passt: anderen Font wählen, nicht den Test ändern.
- [ ] **Step 2: Failing tests:** `ascii_printable_all_have_distinct_nonblank_glyphs` (0x21..=0x7E, jedes ≠ leer), `space_is_blank`, `german_umlauts_and_eszett_present` (`ä ö ü Ä Ö Ü ß` ≠ Ersatzglyphe), `box_drawing_and_rounded_corners_present` (`─ │ ╭ ╮ ╰ ╯ ● █ ▀ ▄ ░ ▒ ▓` ≠ Ersatzglyphe), `unknown_codepoints_return_replacement` (`'\u{1F600}'`, `'中'`, `'\u{0301}'` liefern dieselbe Ersatzglyphe, kein Panic).
- [ ] **Step 3:** `scripts/host_tests.sh font::` → FAIL.
- [ ] **Step 4:** Implementieren: sortierte Tabelle `(char, [u8;16])`, Suche per `binary_search`, sonst Ersatzglyphe.
- [ ] **Step 5:** `scripts/host_tests.sh font::` → PASS.
- [ ] **Step 6:** Commit `feat(l_core): embedded 8x16 font with fallback glyph`.

### Task 4: Theme (`theme.rs`)

**Files:**
- Create: `crates/l_core/src/theme.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Consumes: `Rgb` aus Task 2.
- Produces:
  - `pub fn contrast(a: Rgb, b: Rgb) -> f32` (WCAG-Kontrastverhältnis 1.0–21.0)
  - `pub struct Theme { pub bg: Rgb, pub surface: Rgb, pub text: Rgb, pub muted: Rgb, pub accent: Rgb, pub ok: Rgb, pub warn: Rgb, pub err: Rgb }`
  - `pub const AURORA: Theme` mit Startwerten bg `#0B0E14`, surface `#131824`, text `#D6DEEB`, muted `#7B8AA3`, accent `#7AA2F7`, ok `#9ECE6A`, warn `#E0AF68`, err `#F7768E` (Startwerte; die Tests entscheiden, notfalls heller stellen)

- [ ] **Step 1: Failing tests:** `contrast_black_white_is_21` (±0.01), `contrast_is_symmetric`, `text_on_bg_at_least_7`, `all_other_text_colors_at_least_4_5_on_bg_and_surface` (`muted`, `accent`, `ok`, `warn`, `err` auf `bg` und `surface`), `accent_differs_from_status_colors`.
- [ ] **Step 2:** `scripts/host_tests.sh theme::` → FAIL.
- [ ] **Step 3:** Implementieren (relative Luminanz mit sRGB-Linearisierung); Werte nur so weit anpassen, wie die Tests es verlangen.
- [ ] **Step 4:** `scripts/host_tests.sh theme::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): aurora theme with contrast checks`.

### Task 5: Renderer (`render.rs`)

**Files:**
- Create: `crates/l_core/src/render.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Consumes: `Grid`, `Cell`, `Rgb` (Task 2), `glyph`, `GLYPH_W`, `GLYPH_H` (Task 3).
- Produces:
  - `pub enum PixelFormat { Rgb, Bgr }`
  - `pub struct FbInfo { pub width: usize, pub height: usize, pub stride_px: usize, pub bytes_per_px: usize, pub format: PixelFormat }`
  - `pub fn grid_size_for(info: &FbInfo) -> (usize, usize)` (Spalten = `width / GLYPH_W`, Zeilen = `height / GLYPH_H`)
  - `pub fn render(grid: &Grid, rows: core::ops::Range<usize>, fb: &mut [u8], info: &FbInfo)` (zeichnet nur diese Zeilen; schreibt nie außerhalb von `fb`)

- [ ] **Step 1: Failing tests:** `grid_size_for_1280x720` (`(160, 45)`), `rgb_and_bgr_swap_channels` (eine Zelle mit `fg = Rgb(1,2,3)` ergibt im BGR-Puffer `[3,2,1,..]`), `stride_larger_than_width_does_not_shift_rows` (Pixel (0,1) liegt bei `stride_px*bytes_per_px`), `only_requested_rows_are_touched` (Puffer vorab mit `0xAA`, nur Zeilen `2..3` gerendert, alle anderen Bytes unverändert), `odd_width_leaves_remainder_pixels_untouched`, `buffer_too_small_does_not_panic`, `golden_hash_of_hello_row` (FNV-1a-Hash eines gerenderten `"Hello"` in `AURORA`-Farben; der erwartete Wert wird nach Prüfung der Ausgabe per PPM-Dump im Test eingetragen).
- [ ] **Step 2:** `scripts/host_tests.sh render::` → FAIL.
- [ ] **Step 3:** Implementieren; Schreibzugriffe über `get_mut`, damit zu kleine Puffer nicht paniken.
- [ ] **Step 4:** `scripts/host_tests.sh render::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): pure grid-to-pixels renderer`.

### Task 6: Widgets (`widgets.rs`)

**Files:**
- Create: `crates/l_core/src/widgets.rs`
- Modify: `crates/l_core/src/lib.rs`

**Interfaces:**
- Consumes: `Grid`, `Rect`, `Rgb` (Task 2), `Theme` (Task 4).
- Produces:
  - `pub fn rounded_box(grid: &mut Grid, r: Rect, border: Rgb, fill: Rgb)` (Ecken `╭╮╰╯`, Kanten `─│`; Rechtecke unter 2×2 werden ignoriert)
  - `pub struct HintBar` mit `HintBar::new(hints: &[(&str, &str)]) -> Result<HintBar, TooManyHints>` (mehr als 3 → `Err(TooManyHints)`) und `HintBar::draw(&self, grid: &mut Grid, row: usize, theme: &Theme)`
  - `pub fn status_bar(grid: &mut Grid, row: usize, left: &str, right: &str, theme: &Theme)` (rechter Text rechtsbündig; bei Platzmangel wird `left` gekürzt, `right` bleibt sichtbar)

- [ ] **Step 1: Failing tests:** `rounded_box_has_rounded_corners_and_filled_interior`, `rounded_box_smaller_than_2x2_is_ignored`, `rounded_box_clipped_by_grid_edge_does_not_panic`, `hint_bar_rejects_four_hints`, `hint_bar_accepts_zero_to_three`, `status_bar_keeps_right_text_when_left_too_long`, `status_bar_empty_texts_draw_only_background`.
- [ ] **Step 2:** `scripts/host_tests.sh widgets::` → FAIL.
- [ ] **Step 3:** Implementieren.
- [ ] **Step 4:** `scripts/host_tests.sh widgets::` → PASS.
- [ ] **Step 5:** Commit `feat(l_core): rounded box, hint bar (max 3), status bar`.

### Task 7: Framebuffer-Boot und Konsole im Kernel

Setzt die Entscheidung aus Task 1 um. Beschrieben ist Variante (a); bei (b) passt der Task-1-Report die Dateien an.

**Files:**
- Modify: `Cargo.toml`, `.cargo/config.toml`, `.github/workflows/ci.yml`, `src/main.rs`, `src/lib.rs`, `scripts/qemu_shell_smoke.py`, `README.md`
- Create: `src/fb_console.rs`; ein Image-Builder-Crate laut Task-1-Notiz
- Delete (am Ende des Tasks): `src/vga_buffer.rs`
- Test: `tests/framebuffer.rs`

**Interfaces:**
- Consumes: `render`, `FbInfo`, `Grid`, `AURORA` (Tasks 2–5).
- Produces (gleiche Namen wie bisher in `src/vga_buffer.rs`, damit Aufrufer unverändert bleiben): `fb_console::_print(args: fmt::Arguments)`, `fb_console::emergency_print(args: fmt::Arguments)`, `fb_console::backspace()`, `fb_console::clear_screen()`; zusätzlich `fb_console::init(info: FbInfo, buffer: &'static mut [u8])`.

- [ ] **Step 1: Failing test** `tests/framebuffer.rs`: `console_writes_pixels` (nach `init` und `print!("A")` ist das Pixelmuster der Zelle (0,0) gleich `render` desselben Rasters), `emergency_print_works_while_locked` (Konsole gesperrt halten, `emergency_print` darf nicht blockieren und muss Pixel setzen).
- [ ] **Step 2:** `cargo test --test framebuffer` → FAIL.
- [ ] **Step 3:** `fb_console` implementieren: `spin::Mutex<Console>` mit `Grid`, `render` nach jeder Ausgabezeile nur über `take_dirty()`; `emergency_print` nutzt `try_lock` und fällt auf Serial zurück.
- [ ] **Step 4:** Bootpfad auf Framebuffer umstellen, alle bisherigen Tests und `scripts/qemu_shell_smoke.py` anpassen (Smoke-Test liest statt `pmemsave 0xb8000` jetzt `screendump` und vergleicht Zeilen gegen die mit `l_core::render` auf dem Host berechneten Pixel; Python ruft dazu ein kleines Host-Binary aus `crates/l_core` auf).
- [ ] **Step 5:** `cargo test`, `scripts/host_tests.sh`, `python3 -I scripts/qemu_shell_smoke.py` → alle PASS.
- [ ] **Step 6:** `src/vga_buffer.rs` löschen, `ARCHITECTURE.md` (Tabelle "VGA text output" → Framebuffer), `README.md` und CI aktualisieren.
- [ ] **Step 7:** Commit `feat: framebuffer console replaces VGA text mode`.

### Task 8: Timer auf 100 Hz und Animation (`anim.rs`)

Die PIT läuft heute mit ca. 18 Hz **[assumed]**; für flüssige Animationen sind mindestens 60 Hz nötig.

**Files:**
- Modify: `src/time.rs`, `src/interrupts.rs`, `crates/l_core/src/time.rs` (Tick-Mathematik), alle Stellen mit tick-basierten Budgets in `src/sched.rs` und `src/usermode.rs`
- Create: `crates/l_core/src/anim.rs`
- Test: bestehende `tests/processes.rs`, Host-Tests in `anim.rs`

**Interfaces:**
- Produces: `pub const TICK_HZ: u32 = 100` in `l_core::time`; `pub fn ease_out_cubic(t: f32) -> f32` (0.0→0.0, 1.0→1.0); `pub fn blend(a: Rgb, b: Rgb, alpha: u8) -> Rgb`; `pub struct Fade { pub start: u64, pub duration: u64 }` mit `Fade::alpha(&self, now: u64, reduce_motion: bool) -> u8` (bei `reduce_motion` sofort 255).

- [ ] **Step 1: Failing tests (Host):** `ease_out_cubic_endpoints_and_monotonic`, `blend_zero_is_a`, `blend_255_is_b`, `fade_alpha_ramps_and_clamps`, `fade_reduce_motion_is_immediate`, `ticks_to_ms_uses_100hz` (Anpassung der vorhandenen Tick-Mathematik).
- [ ] **Step 2:** `scripts/host_tests.sh anim::` und `time::` → FAIL.
- [ ] **Step 3:** `anim.rs` implementieren; PIT-Divisor für `TICK_HZ` berechnen, alle Tick-Budgets (CPU-Zeit, Wartezeiten) proportional umrechnen statt hartkodiert zu lassen.
- [ ] **Step 4:** `scripts/host_tests.sh` und `cargo test` (insbesondere `processes`, `usermode`) → PASS.
- [ ] **Step 5:** `ARCHITECTURE.md`: gemessene Tick-Rate unter QEMU eintragen (Test mit Messung), den **[assumed]**-Vermerk ersetzen.
- [ ] **Step 6:** Commit `feat: 100 Hz tick and animation primitives`.

### Task 9: Aurora-Oberfläche in der Shell

**Files:**
- Modify: `user/src/bin/shell.rs`, `src/fb_console.rs`, `src/syscall.rs` (nur wenn ein neuer Aufruf nötig ist)
- Test: `tests/userland.rs`, `scripts/qemu_shell_smoke.py`

**Interfaces:**
- Consumes: `status_bar`, `HintBar`, `rounded_box`, `AURORA`, `Fade` (Tasks 4, 6, 8).

- [ ] **Step 1: Failing test:** `shell_screen_has_status_and_at_most_three_hints` (Smoke-Test-Zeile: Statusleiste in der obersten Zeile enthält "amnesisch: nein" bis M2, Hinweiszeile in der letzten Zeile enthält höchstens drei Einträge).
- [ ] **Step 2:** Test ausführen → FAIL.
- [ ] **Step 3:** Layout: Statusleiste oben (links Name, rechts Systemzustand), Ausgabefläche, Eingabezeile mit Prompt, Hinweiszeile unten (`Enter` ausführen, `Tab` vervollständigen, `Strg+K` Palette – die Palette selbst folgt in M4). Boot-Einblendung mit `Fade`.
- [ ] **Step 4:** `cargo test`, `python3 -I scripts/qemu_shell_smoke.py` → PASS; Screenshot per `screendump` in `docs/` ablegen.
- [ ] **Step 5:** Commit `feat(shell): aurora layout with status and hint bar`.

### Task 10: Dokumentation und Abschluss M1

**Files:**
- Modify: `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, `docs/THREAT_MODEL.md`, `README.md`

- [ ] **Step 1:** ROADMAP: M1 als erledigt eintragen, M2–M6 als Reihenfolge übernehmen; THREAT_MODEL um die Grenzen aus Spec §4 ergänzen.
- [ ] **Step 2:** Nur belegte Aussagen übernehmen (jede mit Verweis auf den Test).
- [ ] **Step 3:** Commit `docs: M1 abgeschlossen, Roadmap M2-M6`.

---

## Folgemeilensteine (Rahmen; je eigene Spec und eigener Plan)

Für jeden gilt: zuerst brainstorming, dann Spec, dann writing-plans. Hier stehen nur die bereits bekannten Eingangsfragen und Abnahmen.

### M2 – Amnesisches Live-System
- Eingangsfragen: xHCI-Treiberumfang (Ports, Hubs, HID und Mass Storage), Image-Format im RAM, Wipe-Reihenfolge nach Spec §5, ACPI-Poweroff ohne ACPI-Bibliothek.
- Abnahme: QEMU `device_del` löst Wipe aus; Test prüft per Speicherabbild, dass Nutzer- und Schlüsselspeicher nach dem Wipe null sind; gemessene Wipe-Dauer wird dokumentiert (ersetzt **[assumed]**).

### M3 – Tor
- Erste Aufgabe ist ein Spike mit Abbruchkriterium: Arti-Bootstrap gegen ein lokales Test-Tor-Netz (z. B. chutney) aus Ring 3 mit eigener Runtime, Sockets und Zeit-Adapter. Scheitert er, wird der Minimal-Client-Rückfall als eigene Spec bewertet.
- Abnahme: Paketmitschnitt zeigt ausschließlich Verkehr zu Guards/Bridges; ein Prozess ohne Tor-Endpoint kann keinen Socket öffnen; Zeitabweichung gegen Consensus blockiert den Aufbau.

### M4 – Terminal-Browser
- Eingangsfragen: HTML-Tokenizer-Strategie (eigener, host-getesteter Parser), Reader-Layout-Regeln, Link-Hints, Onion-Dienste.
- Abnahme: Host-Tests mit kaputtem und feindlichem HTML; Seite öffnen, Link folgen, zurück – Ende-zu-Ende über Tor; Befehlspalette (`Strg+K`) und Adressleiste (`Strg+L`).

### M5 – Echte Hardware
- Eingangsfragen: UEFI-Pfad, Zielgeräteliste, echte NIC (e1000e/RTL8168), Firmware-Eigenheiten bei xHCI.
- Abnahme: Boot und Browsen auf mindestens einem realen Gerät, Wipe beim Abziehen messbar.

### M6 – Härtung
- Verified Boot, WLAN, externes Audit; Spec entsteht erst nach M5.

---

## Selbstprüfung

- **Spec-Abdeckung:** §1/§3 → Meilensteintabelle und M1-Tasks; §4 (Tor-only, Grenzen) → Global Constraints, Task 10, M3; §5 (Wipe) → M2; §6 (Optik, Bedienung) → Tasks 3, 4, 6, 8, 9 (Befehlspalette und Browser-Bedienung sind M4); §7 (Testen) → Host-, Kernel-, Screenshot-Tests je Task; §8 (Risiken) → Task 1 (Boot), M3-Spike. Lücke bewusst: Barrierefreiheit "Farbenblindheit" wird nur über Kontrast und Ein-Akzent-Regel abgedeckt; eine Simulation ist nicht eingeplant.
- **Typkonsistenz:** `Rgb`, `Cell`, `Grid`, `Rect` (Task 2) werden in den Tasks 4–9 unverändert verwendet; `FbInfo`/`PixelFormat` (Task 5) in Task 7; `Fade`/`blend` (Task 8) in Task 9.
- **Proportion:** Der Plan beschreibt M1 mit Signaturen und Testnamen und lässt Funktionskörper weg; M2–M6 bleiben Rahmen, bis ihre Specs existieren.
