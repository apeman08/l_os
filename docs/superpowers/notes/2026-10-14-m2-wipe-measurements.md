# M2: gemessene Wipe-Dauer (Task 24)

Datum: 2026-10-14. Messung mit `python3 -I scripts/qemu_wipe_test.py --measure 256M,1G,4G` (ein `wipe-test`-Build, je Größe beide Auslöser, jeder Lauf mit allen Abbild-Prüfungen von Task 11/24; alle 6 Läufe PASS).

## Umgebung

- Host-CPU: `Intel(R) Xeon(R) Processor @ 2.10GHz` (`/proc/cpuinfo`), 4 logische CPUs, 16 GB RAM.
- QEMU: `QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18)`, OVMF aus dem Ubuntu-Paket.
- Beschleuniger: **TCG** (kein `/dev/kvm`; das Skript setzt kein `-accel`).
- Boot: `--boot usb` (Standard): OVMF bootet das Image von `usb-storage` (`id=stick0`, `readonly=on`, `bootindex=0`) an `qemu-xhci`; kein `usb-kbd`, Tasten (Sentinel, Hotkey) gehen per QMP `send-key` an PS/2.
- Auslöser: `hotkey` = `send-key ctrl alt backspace`; `unplug` = QMP `device_del stick0` (der Kernel erkennt das Abziehen am Stick-Port über die Stick-Wache, `Trigger::StickRemoved`).

## Messwerte

| RAM | Auslöser | Ergebnis | total_us | keys_us | user_us | usable_us | bytes | Host: Befehl -> SHUTDOWN |
|---|---|---|---|---|---|---|---|---|
| 256 MiB | hotkey | PASS | 1032651 | 5101 | 12116 | 1031440 | 245182464 | 1.203 s |
| 256 MiB | unplug | PASS | 1141876 | 4819 | 11834 | 1140703 | 245182464 | 1.110 s |
| 1024 MiB | hotkey | PASS | 3024082 | 6273 | 13682 | 3022913 | 1050488832 | 3.140 s |
| 1024 MiB | unplug | PASS | 4078171 | 7375 | 23146 | 4076936 | 1050488832 | 3.937 s |
| 4096 MiB | hotkey | PASS | 6867961 | 7218 | 15525 | 6866678 | 4271710208 | 6.849 s |
| 4096 MiB | unplug | PASS | 7216102 | 5315 | 13112 | 7214884 | 4271710208 | 7.011 s |

Einzelläufe am selben Tag (256 MiB, außerhalb der Tabelle): hotkey `total_us` 1061498 / 1342941, Host 1.213 s / 1.507 s; unplug `total_us` 1123757 / 1233880, Host 1.080 s / 1.204 s. Streuung zwischen Läufen derselben Sitzung also rund ±20 %.

## Lesart

- Die `*_us`-Werte stammen aus der `WIPE-REPORT`-Zeile: Zeit vom Auslöser bis zum **Ende** der jeweiligen Stufe (kumulativ, daher nicht fallend): `keys_us` bis `KeysZeroed` (Stufe 5), `user_us` bis `UserZeroed` (6), `usable_us` bis `UsableZeroed` (8).
- **`total_us` ist eine Untergrenze**: es endet bei `KernelZeroed` (Stufe 9); die Insel-Nullung, `wbinvd` und das S5-Schreiben (Stufe 11) fehlen. Die Host-Zeit (QMP-Befehl bis zum `SHUTDOWN`-Ereignis, `guest-shutdown`) schließt alles ein, auch die Erkennung (Tasten-ISR bzw. Timer-Poll der Stick-Wache) und die QMP-Latenz.
- Die Gast-Zeiten rechnen TSC-Takte mit einer beim Boot gegen ~50 ms PIT gemessenen Frequenz um. Wie genau diese Kalibrierung unter TCG ist, wurde **nicht gemessen** (die frühere Angabe "auf einige Prozent genau" war eine Annahme); jedenfalls ist `total_us` in einigen Zeilen größer als die Host-Zeit, obwohl es eine Untergrenze sein müsste. Maßgeblich für die Gesamtdauer ist die Host-Spalte.
- Die Dauer wird von Stufe 8 (`UsableZeroed`, alle freien Frames) bestimmt; Schlüssel (Stufe 5) sind nach 5–8 ms genullt, Nutzerprozess-Frames (Stufe 6) nach 12–23 ms.
- 4 GiB: die Speicherkarte hat 9 `Usable`-Bereiche, der höchste endet bei `0x140000000` (5 GiB; QEMU legt 1 GiB oberhalb von 4 GiB ab). `usable_ram_zero_after_wipe` prüft jeden Bereich im Abbild auf Null, also auch den oberhalb der 4-GiB-Bitmap-Grenze: Stufe 8 nullt Speicher oberhalb 4 GiB (Spec §8.2, Risikotabelle "Speicher > 4 GiB").
- Unterschiede hotkey/unplug: nur bei 256 MiB durch Wiederholungen belegt, dort liegen sie innerhalb der Lauf-zu-Lauf-Streuung (±20 %). Bei 1 GiB (3,14 s gegen 3,94 s) und 4 GiB gibt es je nur einen Lauf pro Auslöser; ob der Unterschied dort Streuung ist, ist **nicht belegt**. Dass die Abzieh-Erkennung höchstens einen Timer-Tick (10 ms) kostet, folgt aus dem Entwurf (Polling in jedem Tick, Spec E4), ist aber **nicht gemessen**.

## Nicht übertragbar

Das sind **TCG-Zahlen** (jede Gast-Instruktion wird übersetzt, die Speicherbandbreite ist ein Bruchteil der echten). Sie sagen nichts über die Dauer auf echter Hardware aus und werden hier bewusst nicht hochgerechnet; echte Messungen gehören zu M5 (Spec-Risikotabelle "TCG-Zeiten nicht übertragbar").

Nachtrag (Task 25, 2026-10-15): Wiederholung bei 256 MiB in einer neuen Sitzung (gleiche Host-CPU-Angabe `Intel(R) Xeon(R) Processor @ 2.10GHz`, 4 CPUs, QEMU 8.2.2, TCG, `--boot usb`), je ein Lauf, beide PASS: `--trigger unplug` `total_us` 349684, Host 0.346 s; `--trigger hotkey` `total_us` 389863, Host 0.581 s. Das ist ein Drittel bis die Hälfte der Tabellenwerte: die Streuung zwischen Sitzungen (Host-Last, Nachbarn auf der VM) ist viel größer als ±20 %; die Tabelle zeigt die Größenordnung unter TCG, keinen festen Wert.

Nachtrag (Task 25): Lesart oben abgeschwächt (Streuung nur bei 256 MiB belegt; TSC-Genauigkeit und Erkennungslatenz als nicht gemessen markiert). Die Werte selbst sind unverändert und stehen jetzt auch in Zukunftsplan §5, `docs/THREAT_MODEL.md` und `docs/ROADMAP.md`.

Nachtrag (Fix-Welle nach dem Abschluss-Review, 2026-10-15): neuer Auslöser `--trigger panic` (Kernel-Panik außerhalb eines Wipes, `Trigger::Panic`), ein Lauf bei 256 MiB, gleiche Umgebungsangaben (TCG, `--boot usb`), PASS: `total_us` 1193837, Host (QMP `send-key f12` bis `SHUTDOWN`) 1.163 s. Über alle Läufe bei 256 MiB liegt die Host-Zeit damit zwischen 0,35 s und 1,6 s (höchster Einzelwert 1.507 s, Ledger: 1,08–1,60 s); das README nennt deshalb "0,35–1,6 s" und bleibt bei der TCG-Einschränkung oben.
