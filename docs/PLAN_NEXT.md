# Plan: Wie es weitergeht

Stand: Phase 0–4 umgesetzt (Abweichungen und Lücken in der Tabelle unten); Phase 5–6 offen (siehe „Status“ unten); Rest offen. Grundlage ist `docs/ROADMAP.md`; dieser Plan macht daraus abarbeitbare Aufgaben.

> Hinweis 2026-10-13: Die Phase-0-Zeilen 0.1 und 0.3 nennen `bootimage` und `vga_buffer`; das ist historisch. Seit M1 bootet l_os per UEFI (Cargo-Runner `l_os_run` in `tools/image`; bis 2026-10-13 `scripts/qemu_runner.sh`) und nutzt die Framebuffer-Konsole `src/fb_console.rs` (siehe `docs/ROADMAP.md`).

## Leitregeln für jede Aufgabe
- Test zuerst schreiben, sehen dass er fehlschlägt, dann implementieren (Kernel-Tests in `tests/*.rs`, reine Logik als Host-Test in `crates/l_core`).
- Pro Aufgabe ein Commit; vor dem Commit `cargo test`, `scripts/host_tests.sh` und `python3 -I scripts/qemu_shell_smoke.py` ausführen.
- Jeder neue `unsafe`-Block bekommt einen `// SAFETY:`-Kommentar.
- Nach jeder Phase `docs/ARCHITECTURE.md`, `THREAT_MODEL.md` und `SECURITY.md` aktualisieren. Nichts als verifiziert beschreiben, was kein Test belegt.
- Neue Abhängigkeit nur nach Prüfung: `no_std`, Allokation, `unsafe`, Wartung, Lizenz.

## Phase 0 – Grundlagen und Absicherung (klein, zuerst)
| # | Aufgabe | Dateien | Abnahme |
|---|---|---|---|
| 0.1 | CI einrichten: nightly (pinned), `bootimage`, QEMU installieren; `cargo test`, Host-Tests, Smoke-Test | `.github/workflows/ci.yml` | CI grün auf einem Branch; Fehler in einem Test macht CI rot |
| 0.2 | Handler für #NMI, #MC und SIMD/x87 (fail-stop) | `src/interrupts.rs`, `tests/exception_*.rs` | Test pro erreichbarem Fall |
| 0.3 | Panic-Pfad ohne VGA-Lock (Notausgabe auf Serial, Lock mit `try_lock`) | `src/main.rs`, `src/vga_buffer.rs` | Test: Panic während VGA-Lock gehalten endet nicht in Deadlock |
| 0.4 | `unwrap`/`expect` auf Initialisierungspfaden inventarisieren und in `SECURITY.md` listen | `docs/SECURITY.md` | Liste vollständig per `grep` belegt |

## Phase 1 – Echte Mehrprozess-Isolation (Voraussetzung für alles Weitere)
| # | Aufgabe | Abnahme |
|---|---|---|
| 1.1 | **Frame-Freigabe:** Freelist im Frame-Allocator; `usermode::run` gibt Frames zurück (löst das bekannte Leck) | Test: 1000 `run`-Aufrufe, `frames_allocated()` bleibt beschränkt |
| 1.2 | **Eigene Adressräume:** pro Prozess eine L4-Tabelle; Kernel-Hälfte wird kopiert/geteilt, User-Hälfte ist privat; Wechsel über CR3 | Test: Prozess A kann Daten von Prozess B nicht lesen (gleiche virtuelle Adresse, verschiedener Inhalt) |
| 1.3 | **Prozesstabelle und Kontext:** `Process {id, caps, aspace, kernel_stack, state}`, ein Kernel-Stack pro Prozess, TSS-`rsp0` wird beim Wechsel gesetzt | Test: zwei Prozesse wechseln sich kooperativ ab (`yield`-Syscall) |
| 1.4 | **Präemptiver Scheduler** (Round-Robin über den Timer-Tick), Zeitbudget pro Prozess bleibt | Test: Endlosschleife blockiert einen zweiten Prozess nicht |
| 1.5 | **SMEP/SMAP** (CPUID prüfen, CR4 setzen, `stac/clac` um User-Kopien) | Gesamte `tests/usermode.rs` bleibt grün; Test: Kernel-Zugriff auf User-Seite ohne `stac` faultet |
| 1.6 | Syscall-Argument-Tests (Grenzwerte, alle Längen/Adressen systematisch) und User-Stack-Overflow-Test | Tests grün, keine Kernel-Panic |

Risiken: Kontextwechsel in Assembler ist fehleranfällig; verlassene Interrupt-Frames (siehe `SECURITY.md`) müssen durch einen sauberen Wechselmechanismus ersetzt werden. Entscheidungspunkt nach 1.3: bleibt `int 0x80` oder Umstieg auf `syscall/sysret`?

## Phase 2 – Capabilities als Handles, Shell aus Ring 0
| # | Aufgabe | Abnahme |
|---|---|---|
| 2.1 | Kernel-Objekttabelle pro Prozess: Capability = Handle (Index) auf Objekt mit Rechten; ungültige Handles → Fehler | Host-Test in `l_core` für Tabellenlogik; Kernel-Test: gefälschtes Handle wird abgelehnt |
| 2.2 | Delegation nur explizit (`cap_send` mit abgeschwächten Rechten), Widerruf | Tests: Rechte können nur sinken; Widerruf wirkt auf Kinder |
| 2.3 | **IPC:** synchrone Nachrichten (kleiner Puffer) über Endpoint-Handles | Test: Client/Server-Austausch, Sender ohne Handle scheitert |
| 2.4 | **Konsolen- und Tastaturdienst** als Ring-3-Prozess; Tastatureingabe geht nur an den Prozess mit Fokus-Capability | Test: unfokussierter Prozess erhält keine Tasten |
| 2.5 | **Shell als Ring-3-Prozess** mit nur Konsole + Spawn-Rechten | Smoke-Test unverändert grün; `security` meldet Shell in Ring 3 |

Kernpunkt: Eine Freigabe (Phase 3) darf nur über einen **vertrauenswürdigen Eingabepfad** erfolgen: Nur der Prozess mit `Cap::Policy` und Tastaturfokus kann Netzfreigaben erteilen. Ein unprivilegiertes Programm darf sich nicht selbst freigeben oder den Nutzer täuschen können.

## Phase 3 – Netzwerk-Vorbereitung (erst nach Phase 1 und 2.1–2.4)
| # | Aufgabe | Abnahme |
|---|---|---|
| 3.1 | PCI-Enumeration (Konfigurationsraum, Geräteliste) | Test unter QEMU: virtio-net wird gefunden; Befehl `devices` zeigt es |
| 3.2 | NIC-Wahl dokumentieren und entscheiden (virtio-net vs. e1000/RTL8139) mit Begründung; DMA-Speicher nur aus Kernel-Frames | Eintrag in `ARCHITECTURE.md` |
| 3.3 | NIC-Treiber als **Dienst mit Gerätekapability** (Interface bleibt „down“ ohne Freigabe) | Test: Prozess ohne Gerätecap kann das Gerät nicht ansprechen |
| 3.4 | Netz-Stack `smoltcp` evaluieren (Lizenz, `unsafe`, `no_std`, Alloc-Bedarf) und einbinden | Entscheidung dokumentiert; Test über Loopback |
| 3.5 | **Policy am Syscall-Rand:** `net_connect(host, port)` ruft `NetPolicy::check`; ohne Freigabe kein Paket | Test mit Paketzähler: bei Deny sendet die NIC **kein** Byte |
| 3.6 | **DNS nur nach Freigabe** (DNS gilt als Netzoperation, Resolver-Ziel ebenfalls erlaubnispflichtig) | Test: ohne Freigabe kein DNS-Paket |
| 3.7 | Trusted-Path-UI: `net request host:port` (zeigt Ziel und Operation), `net permissions`, `net revoke`, `net status` – Anzeige unterscheidet **angefragt** von **durchgesetzt** | Smoke-Test: Anfrage → Freigabe → Zugriff → Widerruf → Zugriff scheitert |
| 3.8 | TCP-Verbindung zu einem lokalen Testserver im QEMU-Hostnetz | Test: Verbindungsfehler (Port zu) liefert saubere Fehlermeldung |

Unbekannt/zu prüfen: DMA-Isolation ohne IOMMU (NIC kann beliebigen Speicher lesen/schreiben – das ist ein Vertrauen in die emulierte Hardware, in `THREAT_MODEL.md` festhalten).

## Phase 4 – TLS und HTTPS
| # | Aufgabe | Abnahme |
|---|---|---|
| 4.1 | **Zufallsquelle:** RDRAND/RDSEED prüfen, Fallback dokumentieren (Jitter allein nicht ausreichend); Entropie-Test unter QEMU | Test: Quelle liefert, Ausfall → Verbindung wird verweigert |
| 4.2 | **Uhrzeit** (CMOS-RTC) für Zertifikatsgültigkeit; Misstrauen gegenüber unplausiblen Zeiten | Test: Zertifikat „noch nicht gültig“/„abgelaufen“ wird abgelehnt |
| 4.3 | TLS-Bibliothek evaluieren: `rustls` im `no_std`-Modus mit Krypto-Provider ohne libc-Abhängigkeit; Trust-Store (Wurzelzertifikate) einbetten und Herkunft dokumentieren. **Keine eigene Krypto.** | Entscheidung mit Begründung; Machbarkeit als Spike-Branch belegt |
| 4.4 | TLS-Client über den TCP-Dienst; Zertifikatsprüfung (Kette, Hostname, Gültigkeit) | Tests gegen lokalen Server: gültig ok; falscher Hostname, abgelaufen, selbstsigniert, unbekannte CA → jeweils Abbruch |
| 4.5 | `fetch https://…`: nach TLS-Fehler **kein** Fallback auf HTTP; Antwort wird angezeigt | Akzeptanz erst, wenn der echte Abruf auf dem OS unter QEMU funktioniert |

Risiko: Größter Unsicherheitsfaktor des Projekts. Falls `rustls` ohne `std` nicht tragfähig ist, ist die Alternative ein klar abgegrenzter, geprüfter Port einer anderen Bibliothek – nicht ein Eigenbau.

## Phase 5 – Geheimnisse und optionaler Speicher
| # | Aufgabe | Abnahme |
|---|---|---|
| 5.1 | `Secret<T>`-Typ: Zeroisierung per `zeroize` (geprüfte Crate), kein `Clone`/`Debug`, kurzer Gültigkeitsbereich | Host-Test: Puffer ist nach Drop null; Test auf Fehlerpfade |
| 5.2 | Speicher-Design-Dokument **vor** Code: Passwort, KDF (Argon2id), AEAD, Metadatenformat, Korruptionsverhalten, Einwilligung | Review des Dokuments |
| 5.3 | Blockgerät-Treiber (virtio-blk) mit Gerätecapability, standardmäßig nicht gemountet | Test: ohne explizite Aktivierung wird nie geschrieben (Schreibzähler = 0) |
| 5.4 | Verschlüsseltes Volume, nur nach ausdrücklicher Nutzerzustimmung | Tests: falsches Passwort, manipulierte Blöcke, abgebrochener Schreibvorgang |

## Phase 6 – Textbrowser (erst wenn Phase 3 und 4 laufen)
Als unprivilegierter Prozess mit nur `Net`-Anfragerecht, gleicher Freigabeweg wie alle. Reihenfolge: URL-Parser und HTML-Tokenizer als Host-getestete `l_core`-Module (Fuzz-artige Tests mit kaputtem HTML) → Textdarstellung mit nummerierten Links → `open N`/`back` → sichere Redirects (kein Wechsel zu HTTP, jedes neue Ziel braucht eigene Freigabe) → Anzeige von Hostname und TLS-Status. Keine Cookies, kein JavaScript, kein Verlauf, kein Cache, keine automatischen Nachladungen.

## Reihenfolge und Abhängigkeiten
`0` → `1` → `2.1–2.4` → `3` → `4` → `6`; `5` kann parallel ab Ende `2` laufen. `2.5` ist unabhängig von `3`.

## Offene Entscheidungen für dich
1. **Zielhardware:** Bleibt es bei QEMU/BIOS, oder soll früh UEFI + echte Hardware hinein? (Beeinflusst NIC-Wahl, Boot und Secure Boot.)
2. **Tastaturlayout:** Soll Deutsch (`De105Key`) bleiben oder umschaltbar werden?
3. **Syscall-Mechanismus:** `int 0x80` behalten oder auf `syscall/sysret` umstellen (Entscheidung nach 1.3)?
4. **TLS-Weg:** Soll ich zuerst einen Machbarkeits-Spike für `rustls` ohne `std` machen, bevor Phase 3 Aufwand bindet? (Empfehlung: ja – er entscheidet, ob das Projektziel HTTPS realistisch ist.)

## Status (aktualisiert)
| Aufgabe | Stand | Anmerkung |
|---|---|---|
| 0.1 CI | erledigt | Workflow läuft auf GitHub grün (Läufe für Phase 0, 1.2–1.4, 1.5 verifiziert) |
| 0.2 Exception-Handler | erledigt | #NMI, #MC, #OF, #BR, #NM, x87/SIMD, #AC; #NMI/#OF/#BR per Test, #MC/#NM/#AC/FPU ohne Testauslöser (Code vorhanden, nicht ausgeführt) |
| 0.3 Panic ohne VGA-Lock | erledigt | `emergency_print` getestet; der echte Kernel-Panic-Handler selbst ist nicht als Test ausführbar |
| 0.4 unwrap-Inventar | erledigt | in `SECURITY.md` |
| 1.1 Frame-Freigabe | erledigt | Bitmap erkennt Doppelfreigabe; nur die ersten 4 GiB RAM werden verfolgt |
| 1.2 Adressräume | erledigt | |
| 1.3 Prozesstabelle/Kontext | erledigt | Kernel-Stack pro Prozess; verlassene Stacks beim Kill (s. Threat Model) |
| 1.4 Preemptiver Scheduler | erledigt | Round-Robin auf PIT-Tick, per Mutation geprüft |
| 1.5 SMEP/SMAP | erledigt | nur mit `-cpu max`; Status per `security` sichtbar |
| 1.6 Syscall-/Stack-Tests | erledigt | tabellengetriebener Pointer-Test, kein echtes Fuzzing |
| 2.1 Handles | erledigt | |
| 2.2 Delegation/Widerruf | **teilweise** | Delegation mit Abschwächung ja; **Widerruf bereits weitergegebener Handles fehlt** |
| 2.3 IPC | erledigt | synchron-blockierendes Empfangen, Sender bekommt `WOULD_BLOCK` bei voller Queue |
| 2.4 Konsole/Tastatur als Dienst | **teilweise** | Rust-Userland-Pipeline, ELF-Loader, `spawn`/`wait` fertig; Tastatur und Konsole sind Kernel-Objekte mit Handles (Fokus = wer das Handle hält), **kein** separater Dienstprozess |
| 2.5 Shell in Ring 3 | erledigt | Rust-Shell mit Console/Keyboard/System/Spawner-Handles; Absturz → Neustart durch den Kernel |
| 3.1 PCI | erledigt | Enumeration beim Boot (nur Config-Space-Lesen) |
| 3.2 NIC-Entscheidung | erledigt | virtio-net (legacy); Entscheidung und Begründung in `docs/NETWORK.md` |
| 3.3 NIC als Dienst | **abgewichen** | Treiber und Stack im Kernel (kein IOMMU, 64-Byte-IPC); Netz-Autorität nur über Socket-Handles; TLS in Ring 3 |
| 3.4 smoltcp | erledigt | 0.14.0, 0BSD, `deny(unsafe_code)` |
| 3.5 Policy am Syscall-Rand | erledigt | Test mit leerem QEMU-Mitschnitt; Mutation geprüft |
| 3.6 DNS nur nach Freigabe | erledigt | Name **und** Resolver müssen freigegeben sein |
| 3.7 Trusted-Path-UI | erledigt | `net status/devices/permissions/request/revoke/resolver`; angefragt ≠ durchgesetzt |
| 3.8 TCP-Test | erledigt | Echo-Server auf dem Host, abgelehnte Ports, Widerruf, Ablauf |
| 4.1 Zufall | erledigt | RDSEED/RDRAND ohne Fallback; Qualität nicht prüfbar |
| 4.2 Uhrzeit | erledigt | CMOS-RTC, Plausibilitätsfenster; nicht authentifiziert |
| 4.3 TLS-Bibliothek | erledigt | rustls 0.23.45 + eigener Provider-Klebecode auf RustCrypto (Upstream-Provider ist veraltet) |
| 4.4 TLS-Client + Zertifikatsprüfung | erledigt | Host- und Kernel-Tests für alle Fehlerfälle |
| 4.5 `fetch` | **teilweise** | Funktioniert gegen lokale TLS-Server mit Test-CA; **nicht** gegen eine öffentliche Seite gezeigt (Sandbox fängt TLS ab) |
| 5 – 6 | offen | unverändert |
