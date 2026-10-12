# Zukunftsplan l_os: amnesisches, Tor-geroutetes Terminal-System

Stand: 2026-10-12. Status: Entwurf zur Freigabe. Aussagen sind mit **[assumed]** markiert, wenn nichts sie belegt.

## 1. Ziel und Erfolgskriterien

l_os soll ein hochsicheres und zugleich intuitives Betriebssystem werden, das von einem USB-Stick läuft und beim Abziehen alles vergisst (ähnlich Tails).

**Erstes Gesamtziel:** Ein terminalbasierter Browser, dessen gesamter Netzwerkverkehr über Tor läuft, in einer modernen, hochwertigen Terminal-Optik mit einem Bedienkonzept, das ohne Handbuch verständlich ist.

**Erfolgskriterien**
1. Es gibt keinen Netzwerkpfad außer über den Tor-Dienst. Ein Paketmitschnitt zeigt ausschließlich Verkehr zu Guards oder Bridges.
2. Nach dem Abziehen (oder Notfall-Hotkey) ist kein Nutzerzustand mehr im RAM lesbar, und das System schaltet ab.
3. Eine neue Nutzerin kann ohne Anleitung eine Seite öffnen, einem Link folgen und zurückgehen.
4. Kein Bildschirm zeigt mehr als eine Aufgabe und höchstens drei Tastenhinweise.

## 2. Annahmen und Entscheidungen

| Entscheidung | Wahl | Begründung |
|---|---|---|
| Zielplattform | Erst QEMU, dann echte Hardware (M5) | Geringstes Risiko; Abziehen ist in QEMU über `device_del` testbar |
| Tor | Arti portieren, Spike zuerst | Geprüfter Code, keine eigene Krypto |
| Rückfall bei negativem Spike | Enger, geprüfter Minimal-Client (separat zu bewerten) | Nur wenn der Spike scheitert |
| Kernel | Bestehender Rust-Kernel mit Capabilities bleibt | Siehe `docs/ARCHITECTURE.md` |
| Nutzerdaten | Nur RAM, kein Schreiben auf Datenträger | Voraussetzung für den Reset |

Offen und im jeweiligen Meilenstein-Plan zu entscheiden: Font (eingebettetes Bitmap- oder Vektor-Font, Lizenz), WLAN-Strategie (M6), Bridge-Konfiguration ohne Persistenz.

## 3. Zerlegung in Meilensteine

Jeder Meilenstein bekommt eine eigene Spec, einen eigenen Plan und eigene Umsetzung. Dieser Plan beschreibt M1 bis M6 auf Zielebene.

| # | Meilenstein | Ergebnis | Abnahme |
|---|---|---|---|
| M1 | Grafik-Terminal "Aurora" | Framebuffer-Renderer, Zellenraster mit TrueColor und Unicode, Rahmen, Animationen, Designsystem | Pixel-Hash-Screenshot-Tests in QEMU, bestehender Smoke-Test bleibt grün |
| M2 | Amnesisches Live-System | xHCI-Treiber (Massenspeicher beim Boot, Tastatur), Image komplett im RAM, Wipe beim Abziehen und per Hotkey | QEMU `device_del` löst Wipe aus; danach enthält der RAM keinen Nutzerzustand |
| M3 | Tor | Arti-Spike, dann Port als Ring-3-Dienst, einziger Netzpfad | Paketmitschnitt: nur Guards/Bridges; Test: andere Prozesse können keinen Socket öffnen |
| M4 | Terminal-Browser | HTML zu Text im Reader-Layout, Link-Hints, Prozess pro Tab, kein JavaScript | Host-Tests mit kaputtem HTML; Ende-zu-Ende über Tor |
| M5 | Echte Hardware | UEFI-Boot, echte NIC, xHCI auf realen Chipsätzen | Boot und Browsen auf mindestens einem realen Gerät |
| M6 | Härtung | Verified Boot, WLAN, externes Audit | später spezifiziert |

Abhängigkeiten: M1 und M2 sind voneinander unabhängig. M3 braucht die bestehende Netz-/TLS-Basis (Phase 3 und 4), nicht M1 oder M2. M4 braucht M1 und M3. M5 braucht M2 und M4 (Abnahme: Browsen auf realem Gerät). Reihenfolge: M1, M2, M3, M4, M5, M6.

## 4. Sicherheitsmodell

- **Tor-only durch Konstruktion:** Nur der Tor-Dienst hält ein Netz-Handle. Der Browser besitzt nur einen Endpoint zu diesem Dienst und kann keinen Socket öffnen. Der Kernel erlaubt dem Tor-Dienst nur Ziele aus Guard- und Bridge-Listen (bestehende `NetPolicy`, default deny). DNS läuft nur über Tor.
- **Zeit:** Die RTC ist unauthentisch. Tor prüft die Zeit gegen den Consensus; unplausible Abweichung verhindert den Verbindungsaufbau.
- **Kein Fallback:** Fällt Tor aus, gibt es keine Verbindung ohne Tor. Der Browser zeigt den Zustand an.
- **Browser:** Kein JavaScript, keine Cookies, kein Verlauf, kein Cache, keine automatischen Nachladungen, nur HTTPS bzw. Onion-Dienste.
- **Grenzen (in `THREAT_MODEL.md` festhalten):** Manipulierte Firmware, Hardware-Keylogger und DMA-Angriffe sind nicht abgedeckt; Cold-Boot-Angriffe werden nur durch den Wipe gemindert. Tor garantiert keine Anonymität gegen Fingerprinting oder globale Beobachter. Ohne IOMMU vertraut das System der NIC.

## 5. Reset beim Abziehen (M2)

1. Beim Boot wird das Image vollständig in den RAM kopiert; danach ist der Stick nicht mehr nötig.
2. Der xHCI-Treiber meldet eine Port-Status-Änderung des Sticks.
3. Der Kernel sperrt sofort die Eingabe und schwärzt den Bildschirm.
4. Zeroize in Prioritätsreihenfolge: Schlüssel und Tor-Zustand, dann alle Nutzerprozesse, dann alle freien und belegten Frames.
5. ACPI-Poweroff, ersatzweise Reset.
6. Der Notfall-Hotkey löst denselben Ablauf aus.

Die Dauer hängt von der RAM-Größe ab und ist **[assumed]** im Bereich einiger Sekunden; M2 misst sie. Ein Abziehen während des Boots führt zu Halt ohne Nutzerzustand.

## 6. Bedienung und Optik

- **Ein Bildschirm, eine Aufgabe.** Statt Menüs gibt es eine Befehlspalette (`Strg+K`) mit unscharfer Suche. Eine Hinweiszeile zeigt kontextabhängig höchstens drei Tasten.
- **Statusleiste:** Tor-Verbindung (●), Länge des Schaltkreises, "amnesisch: ja".
- **Browser:** Reader-Spalte mit angenehmer Zeilenlänge, Links per Buchstaben-Hints, Adressleiste mit `Strg+L`, `Backspace`/`Alt+←` für zurück.
- **Optik:** Dunkles Theme mit genau einem Akzent, abgerundete Boxen, ein hochwertiger eingebetteter Font, sanfte Animationen (Boot, Übergänge). Zwei Signatur-Elemente: Live-Visualisierung des Tor-Schaltkreises beim Verbindungsaufbau und eine "Vergessen"-Animation beim Wipe.
- **Barrierefreiheit:** Kontrastverhältnis und Farbenblindheit werden im Designsystem geprüft; Animationen sind abschaltbar.

## 7. Testen

- Reine Logik (Parser, Layout, Policy, Wipe-Reihenfolge) als Host-Tests in `l_core`.
- Kernel-Tests in QEMU für Treiber und Isolation.
- Screenshot-Tests über QEMU-Monitor (Pixel-Hash) für die Optik.
- Paketmitschnitt-Tests für das Tor-only-Versprechen, Mutationsprüfung der Schutzmaßnahmen wie bisher.
- Nichts wird als verifiziert beschrieben, was kein Test belegt.

## 8. Risiken

| Risiko | Auswirkung | Gegenmaßnahme |
|---|---|---|
| Arti baut nicht ohne Betriebssystem-Std | M3 blockiert | Spike mit klaren Abbruchkriterien; Rückfall getrennt bewerten |
| xHCI auf realer Hardware verhält sich anders | M5 verzögert | Früh auf mehreren Geräten testen |
| Wipe-Dauer zu lang | Daten kurz lesbar | Prioritätsreihenfolge; Messung in M2 |
| Font-/Rendering-Aufwand ohne `std` | M1 verzögert | Bitmap-Font zuerst, Vektor später |
| Umfang | Projekt versandet | Meilensteine einzeln liefern, jeder für sich nutzbar |
