# Funktionsabgleich & Roadmap

Abgleich mit modernen Helpdesk-/Callcenter-Systemen (Zendesk Suite, Freshdesk, HubSpot Service Hub).
✅ fertig & getestet · 📋 geplant

## Agenten-Arbeitsplatz
| Funktion | Status |
|---|---|
| Ticketansicht mit Antwort / interner Notiz, Makros, Audit-Log | ✅ |
| Ansichten (Views) mit Live-Zählern | ✅ |
| Volltextsuche mit Filtern (`status:` `tag:` `assignee:` …) | ✅ |
| Sortieren per Klick auf Spaltenköpfe (inkl. SLA-Dringlichkeit) | ✅ Phase 1 |
| Ad-hoc-Filter in Listen (Status, Priorität, Bearbeiter, Gruppe, Kanal, Text) | ✅ Phase 1 |
| Seitenweise Anzeige (50 pro Seite) | ✅ Phase 1 |
| Massenbearbeitung (Status, Priorität, Zuweisung, Gruppe, Makro) | ✅ Phase 1 |
| Schutz vor verlorenen Änderungen (nur geänderte Felder werden gespeichert) | ✅ |
| Tickets zusammenführen | ✅ Phase 2 |
| Dateianhänge (Agenten & Kunden, sichere Auslieferung) | ✅ Phase 2 |
| Eigene Ticketfelder (Text, Zahl, Auswahl, Ja/Nein, Datum; Pflicht beim Lösen) | ✅ Phase 2 |
| Verknüpfte Tickets (Problem ↔ Vorfälle), Folgeticket zu geschlossenem Ticket | 📋 |
| Kollisionserkennung („Anna bearbeitet gerade“), Echtzeit-Updates | 📋 |
| Tastaturkürzel, persönliche Makros, Rich-Text, @Erwähnungen, CC | 📋 |
| Zeiterfassung pro Ticket | 📋 |

## Routing & Automatisierung
| Funktion | Status |
|---|---|
| Trigger (ereignisbasiert), Makros, SLA-Richtlinien | ✅ |
| **Grafischer Regel-Baukasten** für Ansichten, Makros, Trigger, Automationen, SLAs | ✅ Phase 2 |
| Agentenstatus (Online / Abwesend / Offline) | ✅ Phase 1 |
| Automatische Zuweisung je Gruppe (Round Robin / geringste Auslastung, Kapazität, Warteschlange) | ✅ Phase 1 |
| Zeitgesteuerte Automationen (konfigurierbar, z. B. Erinnerung, Auto-Schließen) | ✅ Phase 2 |
| Geschäftszeiten & Feiertage für SLAs | ✅ Phase 3 |
| SLA-Eskalationen (Benachrichtigung vor Ablauf) | 📋 |
| Skill-basiertes Routing (Sprache, Produkt) | 📋 |
| Webhooks (signiert, mit Wiederholung und Zustellprotokoll) | ✅ Phase 3 |

## Kundenseite
| Funktion | Status |
|---|---|
| Agent legt Ticket für Kunden an (inkl. Anruferkennung per Telefonnummer) | ✅ |
| Kundenportal: Anmeldung/Registrierung, eigene Anfragen, Antworten, Dateien, „gelöst“ | ✅ Phase 1 |
| Kundenzufriedenheit (CSAT-Bewertung nach Lösung, Quote in Reports) | ✅ Phase 2 |
| Kunden zusammenführen (Dubletten) | ✅ Phase 2 |
| E-Mail: Versand per SMTP, Eingang per Webhook, Vorlagen, Postausgang | ✅ Phase 3 |
| E-Mail-Eingang direkt per IMAP-Abruf | 📋 |
| Passwort zurücksetzen per E-Mail (Agenten & Kunden) | ✅ Phase 3 |
| Wissensdatenbank & öffentliches Hilfe-Center, Vorschläge für Kunden und Agenten | ✅ Phase 3 |
| Web-Widget / Live-Chat | 📋 |
| Telefonie: Anruf-Banner, Anrufprotokoll im Ticket, Rückruf-Tickets, Click-to-Call, Aufzeichnungs-Link (sipgate, Twilio, allgemeiner Webhook, Simulator) | ✅ |
| Telefonie: Warteschlangen/IVR, Weiterleiten, Softphone im Browser (WebRTC) | 📋 Backlog |

## Reporting
| Funktion | Status |
|---|---|
| Dashboard: neue/gelöste Tickets, Backlog, Ø Erstantwort, Ø Lösung, SLA-Quote, CSAT | ✅ Phase 1/2 |
| Verlauf pro Tag, Verteilung nach Kanal/Priorität, Agenten-Statistik | ✅ Phase 1 |
| CSV-Export (Excel-kompatibel) | ✅ Phase 1 |
| Benutzerdefinierte Berichte, geplante Berichte per E-Mail | 📋 |

## Kunden und Mitarbeitende getrennt ✅
- „Kunden“ zeigt nur Endkunden (Liste, Suche, Anruferkennung, Firmen); Mitarbeitende werden ausschließlich unter Verwaltung → Team gepflegt.
- Im Kundenformular gibt es keine Rollenänderung mehr; Mitarbeiter-Profile sind über die Kundenansicht weder sichtbar noch änderbar.
- Datenmodell: weiterhin eine Personen-Tabelle mit Rollen (wie Zendesk), weil Tickets/Kommentare auf beide verweisen. 📋 Optional später: getrennte Tabellen, falls fachlich nötig.

## Backlog: Mehrere Projekte (Mandanten)
Ziel: getrennte Projekte, z. B. „Demo“ und „Service“. Entscheidungen bereits getroffen:

- **Komplett getrennt:** jedes Projekt hat eine eigene Datenbank (Tickets, Kunden, Regeln, Gruppen, Felder, Reports). Eine kleine zentrale Datenbank hält nur Projektliste und Agenten-Logins.
- **Ein Login mit Projektwechsler:** Agenten melden sich einmal an und wechseln oben zwischen ihren Projekten. Zugriff hat, wer im Projekt als aktiver Agent angelegt ist.
- **Neues Projekt** durch den Inhaber: wahlweise mit Demodaten oder leer mit Grundausstattung (Ansichten, Makros, Trigger, SLA).
- **Kundenportal und API pro Projekt** erreichbar, z. B. `/p/service/portal`, `/p/service/api/v2/...`.
- **Bestehende Datenbank** wird beim Umbau automatisch zum „Demo-Projekt“, bestehende Konten bleiben gültig.

Umsetzungsskizze: zentrale DB mit `projects`, `global_users`, `agent_sessions`; Projektauflösung per Präfix `/p/{slug}` bzw. Cookie; ein Extractor liefert pro Request die Projekt-Datenbank; Hintergrundjobs (Routing, Automationen) laufen über alle Projekte; Seed wird in Grundausstattung und Demodaten geteilt.

## Verwaltung & Sicherheit
| Funktion | Status |
|---|---|
| Rollen Agent/Admin, Gruppen, Organisationen mit Domain-Zuordnung | ✅ |
| CSRF-Schutz, API-Tokens, Zwei-Faktor-Anmeldung (TOTP), Profilseite | ✅ Phase 3 |
| SSO (SAML/OIDC) | 📋 |
| DSGVO: Datenauskunft (Export) und Anonymisierung | ✅ Phase 3 |
| Mehrsprachigkeit | 📋 |
