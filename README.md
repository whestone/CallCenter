# CallCenter

Helpdesk-/Callcenter-System in Rust nach dem Vorbild von **Zendesk Support**:
Tickets, Anruferfassung, Views, Makros, Trigger, SLAs, Audit-Log und eine
Zendesk-ähnliche REST-API.

**Stack:** axum · tokio · sqlx (PostgreSQL) · askama · htmx

## Starten

```bash
cp .env.example .env        # DATABASE_URL anpassen
cargo run
```

Auf diesem Rechner einfach **`start.cmd` doppelklicken**: startet bei Bedarf die Datenbank, kompiliert
falls nötig, startet den Server und öffnet den Browser. Das Fenster offen lassen, solange CallCenter
laufen soll; Schließen oder Strg+C beendet den Server, die Datenbank läuft weiter. Läuft CallCenter
schon, öffnet `start.cmd` nur den Browser. Probleme (Datenbank startet nicht, Port belegt,
Kompilierfehler) werden im Fenster mit Hinweis angezeigt.

Danach <http://127.0.0.1:3000> öffnen. Beim ersten Start mit leerer Datenbank
werden Migrationen ausgeführt und Startdaten angelegt:

| Rolle | E-Mail | Passwort |
|---|---|---|
| Admin | `admin@example.com` | `admin123` |
| Agent | `agent@example.com` | `agent123` |

(Über `SEED_*` in `.env` änderbar, gilt nur für die allererste Initialisierung.)

### Lokale Datenbank auf diesem Rechner

PostgreSQL 17 liegt portabel unter `%USERPROFILE%\pgsql17`.

```powershell
# starten
& "$env:USERPROFILE\pgsql17\pgsql\bin\pg_ctl.exe" -D "$env:USERPROFILE\pgsql17\data" -l "$env:USERPROFILE\pgsql17\pg.log" start
# stoppen
& "$env:USERPROFILE\pgsql17\pgsql\bin\pg_ctl.exe" -D "$env:USERPROFILE\pgsql17\data" stop
```

Alternativ per Docker: `docker compose up -d` (siehe `docker-compose.yml`).

## Funktionsumfang

### Tickets (Zendesk-Lebenszyklus)
- Status `new → open → pending → hold → solved → closed`, Priorität, Typ
  (question/incident/problem/task), Kanal (web/email/phone/api/chat), Tags, eigene Felder (JSON)
- Öffentliche Antworten und **interne Notizen**
- Eingebaute Regeln wie bei Zendesk:
  - neues Ticket wird bei Zuweisung oder erster Agentenantwort `open`
  - Kundenantwort öffnet `pending`/`solved`/`hold` wieder
  - Bearbeiter ohne Gruppe bekommt seine erste Gruppe
  - Organisation folgt dem Anfragenden
  - gelöste Tickets werden nach 4 Tagen automatisch `closed` (Automation)
- **Audit-Log** jeder Änderung (Feldänderungen, Kommentare, ausgeführte Makros/Trigger, Benachrichtigungen)

### Callcenter
- **„Anruf erfassen“**: Anrufer per Telefonnummer, E-Mail oder Name suchen (Live-Vorschläge,
  inkl. Anzahl offener Tickets); unbekannte Anrufer werden automatisch angelegt
- Kontextspalte im Ticket mit Kundendaten, Click-to-Call-Link und weiteren Tickets des Kunden
- Telefonnummern werden einheitlich international gespeichert, damit Anrufer unabhängig von der
  Schreibweise wiedererkannt werden: `0171 999-888`, `+49 (0) 171 999888`, `0049171999888` → `+49171999888`
  (Landesvorwahl über `DEFAULT_COUNTRY_CODE`, Standard `49`)
- Organisationszuordnung über E-Mail-Domains

### Telefonie (Admin → Telefonie)
- **Anruf-Banner (Screen-Pop)**: eingehende Anrufe erscheinen bei allen Agenten mit Status „Online“
  mit Name, Organisation und Zahl offener Tickets; Buttons „Kundenakte“ und „Ticket anlegen“
  (übernimmt den Anrufer und verknüpft das Gespräch)
- Jedes Gespräch wird mit Richtung, Agent, Dauer und ggf. Aufzeichnung als interne Notiz im Ticket
  protokolliert; Anrufliste am Ticket, beim Kunden und im Admin-Protokoll
- **Verpasste Anrufe** → Rückruf-Ticket (Priorität hoch, Tags `verpasster_anruf`, `rueckruf`),
  unzugewiesen, damit das Routing verteilt; weitere verpasste Anrufe ergänzen dasselbe Ticket
- **Click-to-Call** an Ticket und Kunde: über sipgate (`SIPGATE_TOKEN_ID`/`SIPGATE_TOKEN`, Gerät =
  Durchwahl des Agenten unter Team) oder ohne Anbieter per `tel:`-Link ans Softphone
- Anbieter-Webhooks (aktiv, sobald `TELEPHONY_SECRET` mit mind. 16 Zeichen gesetzt ist):
  `POST /telephony/sipgate?secret=…` (sipgate.io Push-API), `POST /telephony/twilio?secret=…`
  (Twilio Status-Callback), `POST /api/v2/telephony/events?secret=…` (allgemeines JSON für 3CX,
  Asterisk o. ä.: `{"event":"ringing|answered|ended","call_id":"…","from":"…","agent":"…","duration":42}`)
- **Simulator** in der Verwaltung: Testanrufe klingeln lassen, annehmen und auflegen – ohne Anbieter

### Zusammenarbeit im Team
- **Kollisionserkennung**: Im Ticket sieht man, wer es gerade geöffnet hat oder schreibt; neue Antworten
  und Änderungen anderer werden live angezeigt. Hat ein Kollege inzwischen öffentlich geantwortet, wird
  die eigene Antwort nicht verschickt, sondern mit erhaltenem Text und Hinweis wieder angezeigt
- Ticketlisten aktualisieren sich alle 30 Sekunden selbst (nicht, solange man auswählt oder tippt)
- **Benachrichtigungen** (Glocke oben rechts, `/notifications`): @Erwähnung (auch per E-Mail),
  Zuweisung, Kundenantwort auf eigene Tickets, SLA-Warnung/-Verletzung (auch per E-Mail)
- **@Erwähnungen** mit Vorschlagsliste beim Tippen von „@“
- **Verknüpfte Tickets** (Karte rechts im Ticket): Ticket-Nr. eingeben und Art wählen – „verwandt“
  oder „ist Vorfall von …“ (das andere Ticket wird zum Problem). Wird ein Problem gelöst, werden alle
  offenen Vorfälle mitgelöst und erhalten die Lösungsantwort. Verknüpfungen lassen sich per ✕ aufheben
- **Folgetickets** zu geschlossenen Tickets (Button im Ticket; Antwort-Mails auf geschlossene
  Tickets werden automatisch zu Folgetickets)
- **CC**: weitere Personen erhalten öffentliche Antworten per E-Mail
- **SLA-Eskalation**: Warnung `SLA_WARN_MINUTES` (Standard 30) vor Ablauf und Meldung bei Verletzung an
  Bearbeiter, sonst Gruppe, sonst Administratoren – je SLA-Ziel nur einmal

### Arbeitsplatz-Komfort
- **Formatierung** in Antworten: fett, kursiv, Listen, Links, Code (Leiste über dem Antwortfeld,
  Strg+B / Strg+I); HTML wird nie übernommen
- **Tastenkürzel** – `?` zeigt die Übersicht: `j`/`k` durch Listen, `o` öffnen, `x` auswählen,
  `r` antworten, `i` interne Notiz, `n` neues Ticket, `a` Anruf, `/` Suche, `g t`/`g k`/`g w`/`g b`
- **Persönliche Makros** (`/me/macros`, Profil → Meine Makros): nur für einen selbst sichtbar
- **Zeiterfassung**: Timer läuft im Ticket mit (pausiert im Hintergrund-Tab), Wert überschreibbar;
  Summe je Ticket und eigener Anteil in der Seitenleiste

### Live-Chat (`/chats`)
- Widget für die eigene Webseite: `<script src="https://IHR-SERVER/static/chat-widget.js" async></script>`
  (Demo: `/chat/demo`). Besucher geben Name, E-Mail und Frage ein – daraus wird ein Ticket (Kanal „Chat“)
- Agenten sehen wartende Chats mit Zähler in der Seitenleiste, nehmen sie an und antworten im normalen
  Ticket; der Besucher sieht Antworten sofort, beide Seiten sehen „schreibt gerade“
- Solange der Besucher im Chat ist, gehen keine zusätzlichen E-Mails raus; verlässt er ihn oder wird
  der Chat beendet, laufen Antworten per E-Mail weiter. Ist niemand online, wird die Nachricht zur
  normalen Anfrage (Tag `chat_offline`). Verlassene Chats schließen sich nach 30 Minuten

### Business-Regeln
Gemeinsame Regel-Engine (`src/rules.rs`) im Zendesk-Format:

```json
{ "all": [ { "field": "status", "operator": "less_than", "value": "solved" } ],
  "any": [ { "field": "tags", "operator": "includes", "value": "vip urgent" } ] }
```

- **Ansichten**: gefilterte Ticketlisten mit Live-Zählern (Sidebar aktualisiert sich alle 30 s)
- **Makros**: Aktionen per Klick, mit Platzhaltern wie `{{ticket.id}}`, `{{current_user.name}}`
- **Trigger**: laufen bei Create/Change, werden wie bei Zendesk bis zur Stabilität erneut geprüft
  (jeder höchstens einmal pro Update); Operatoren `changed`, `changed_to`, `changed_from` …
- **SLA-Richtlinien**: Erstantwort- und Lösungszeit je Priorität, Anzeige als Countdown/Verletzt

Felder: `status, priority, type, channel, subject, description, assignee_id, group_id,
requester_id, organization_id, tags, sla_policy_id, hours_since_created, hours_since_updated,
update_type, comment_is_public, comment_text, current_user_role, custom_fields.<key>`

Aktionen: `status, priority, type, assignee_id, group_id, subject, set_tags, add_tags,
remove_tags, comment_value, comment_private, notify_requester, custom_fields.<key>`

### Arbeitsplatz & Routing
- Ticketlisten: Filterleiste, Sortieren per Spaltenkopf, 50 pro Seite, Massenbearbeitung
- Agentenstatus Online/Abwesend/Offline (oben rechts); Anmeldung = online, Abmeldung = offline
- Automatische Zuweisung je Gruppe (Admin → Gruppen): Reihum oder geringste Auslastung,
  mit Kapazität pro Agent und Warteschlange, die alle 20 s bzw. beim Online-Gehen verteilt wird
- Tickets und Kunden zusammenführen, Dateianhänge (max. 5 × 10 MB, HTML/SVG nie im Browser ausgeführt)
- Eigene Ticketfelder (Admin → Ticketfelder), optional Pflicht beim Lösen
- **Routing nach Fähigkeiten** (Admin → Fähigkeiten): Eine Fähigkeit wird an Tags erkannt (z. B.
  „Englisch“ ↔ `englisch`); solche Tickets gehen zuerst nur an Agenten mit dieser Fähigkeit (Team → Agent).
  Ist nach `SKILL_TIMEOUT_MINUTES` (Standard 30) niemand Passendes frei, wird normal verteilt

### Regeln grafisch bauen
Ansichten, Makros, Trigger, Automationen und SLAs werden im Admin-Bereich per Klick
zusammengestellt (Feld → Operator → Wert); die JSON-Ansicht bleibt für Fortgeschrittene.
Automationen laufen jede Minute zeitgesteuert (z. B. Erinnerung nach 3 Tagen „Wartend“,
gelöste Tickets nach 4 Tagen schließen).

### Kundenportal (`/portal`)
Kunden melden sich an oder registrieren sich, stellen Anfragen, antworten, laden Dateien hoch,
markieren Anfragen als gelöst und bewerten die Lösung (CSAT). Agenten können einem Kunden
im Kundenprofil einen Portalzugang einrichten. Demo: `max@acme.example` / `kunde1234`.

### Reports (`/reports`)
Kennzahlen (neu, gelöst, Backlog, Ø Erstantwort, Ø Lösung, SLA-Quote, Zufriedenheit),
Verlauf pro Tag, Verteilung nach Kanal/Priorität, Agenten-Statistik, CSV-Export.
**Eigene Berichte** (`/reports/custom`): Kennzahl (neue/gelöste/offene Tickets, Ø Erstantwort, Ø Lösung,
Zufriedenheit, erfasste Zeit) × Aufteilung (Tag/Woche/Monat/Wochentag, Status, Priorität, Kanal, Typ,
Gruppe, Bearbeiter, Organisation, Tag) × Filter; als Balken und Tabelle, CSV, für alle speicherbar.

### E-Mail (Admin → E-Mail)
- Kunden erhalten öffentliche Antworten und Trigger-Benachrichtigungen per Mail; Betreff mit `[#123]`
- Versand per SMTP (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD`, `SMTP_FROM`, `SMTP_SECURITY`);
  ohne Konfiguration landen Mails im Postausgang mit Status „nicht gesendet“
- Eingang: `POST /api/v2/inbound/email?secret=…` mit JSON `{from, from_name, subject, text}`
  (z. B. von Mailgun/Postmark weitergeleitet). Antworten mit `[#123]` vom Anfragenden werden dem Ticket
  zugeordnet, Zitate abgeschnitten; alles andere wird ein neues Ticket. Secret: `INBOUND_EMAIL_SECRET`
- „Passwort vergessen?“ für Agenten und Kunden (Einmal-Link, 1 Stunde gültig); `PUBLIC_URL` für Links

### E-Mail-Eingang per IMAP
- Ungelesene Mails im Postfach werden jede Minute abgeholt (`IMAP_HOST`, `IMAP_PORT` (993),
  `IMAP_USER`, `IMAP_PASSWORD`, `IMAP_FOLDER` (INBOX), `IMAP_POLL_SECONDS`) und danach als gelesen
  markiert: neue Anfrage → Ticket, Antwort mit `[#123]` im Betreff → Kommentar, Anhänge werden übernommen
- Abwesenheitsnotizen (`Auto-Submitted`), Massenmails und eigene Mails werden ignoriert
- Status des letzten Abrufs unter Admin → E-Mail

### Geschäftszeiten (Admin → Geschäftszeiten)
SLA-Richtlinien können in Geschäftszeiten rechnen (Häkchen im SLA-Editor): Nächte, Wochenenden und
Feiertage zählen nicht. Bundesweite Feiertage 2026/2027 sind vorbelegt.

### Wissensdatenbank (`/kb`) & Hilfe-Center (`/portal/help`)
Artikel mit Kategorien, Entwurf/veröffentlicht. Öffentliches Hilfe-Center mit Volltextsuche (deutsche
Wortstämme), „War das hilfreich?“, Vorschläge beim Stellen einer Anfrage und im Ticket für Agenten.

### Webhooks (Admin → Webhooks)
Ereignisse `ticket.created`, `ticket.updated`, `ticket.solved` als JSON-POST, signiert mit
`X-CallCenter-Signature: sha256=<HMAC-SHA256(secret, body)>`, bis zu 5 Wiederholungen.

### Sicherheit & Datenschutz
- CSRF-Schutz (Origin/Referer-Prüfung) für alle schreibenden Anfragen
- Profilseite (`/me`): Passwort ändern, API-Tokens (`Authorization: Bearer cc_…`), Zwei-Faktor-Anmeldung (TOTP)
- Konten mit 2FA dürfen die API nur mit Token nutzen; Admins können 2FA zurücksetzen
- DSGVO im Kundenprofil (Admins): Datenauskunft als JSON, Anonymisieren

### Suche
- **Suchfeld oben**: Vorschläge beim Tippen (Tickets, Kunden, Firmen, Wissensartikel), mit ↑/↓ bedienbar;
  `#12` + Enter öffnet das Ticket direkt
- **Ergebnisseite** (`/search`): Treffer mit hervorgehobenem Textausschnitt, Sortierung nach Relevanz,
  Aktualisierung oder Alter; Schnellsuchen und Syntaxhilfe bei leerer Suche
- Syntax: `drucker toner` (alle Wörter), `"kein internet"` (Phrase), `-tag:spam` / `-drucker` (ausschließen),
  `status:unsolved`, `priority:`, `type:`, `channel:`, `tag:`, `assignee:me|none|Name`,
  `requester:"Max Mustermann"`, `group:`, `organization:`,
  `created>2026-10-01`, `updated<7d`, `solved:heute` (Operatoren `: > < >= <=`)

### Oberfläche
- Dunkelmodus: folgt dem System, Umschalter ◐ oben rechts (wie System → dunkel → hell)
- Kurze Bestätigung nach dem Speichern, Zahl ungelesener Benachrichtigungen im Tab-Titel
- Deutlicher Fokusrahmen für die Bedienung per Tastatur

## REST-API (`/api/v2`)

Authentifizierung per HTTP Basic (E-Mail:Passwort eines Agenten) oder Session-Cookie.

| Methode | Pfad | |
|---|---|---|
| GET/POST | `/tickets` | Liste (`?status=&assignee_id=&requester_id=&page=&per_page=`) / anlegen |
| GET/PUT/DELETE | `/tickets/{id}` | anzeigen / ändern (inkl. `comment`) / löschen (Admin) |
| GET | `/tickets/{id}/comments`, `/audits`, `/sla` | |
| POST | `/tickets/{id}/macros/{macro_id}/apply` | Makro anwenden |
| GET/POST | `/users`, GET `/users/me`, `/users/search?query=` | |
| GET/PUT | `/users/{id}`, GET `/users/{id}/tickets/requested` | |
| GET/POST | `/organizations`, GET/PUT `/organizations/{id}` | |
| GET/POST | `/groups` | |
| GET | `/views`, `/views/{id}/tickets`, `/macros`, `/triggers`, `/slas/policies` | |
| GET | `/search?query=` | |

Beispiel:

```bash
curl -u agent@example.com:agent123 -H "Content-Type: application/json" \
  -d '{"ticket":{"subject":"Rückruf gewünscht","comment":{"body":"Bitte zurückrufen"},
       "requester":{"name":"Eva Test","phone":"+49 40 555"},"channel":"phone","priority":"high"}}' \
  http://127.0.0.1:3000/api/v2/tickets

curl -u agent@example.com:agent123 -X PUT -H "Content-Type: application/json" \
  -d '{"ticket":{"status":"pending","comment":{"body":"Wir melden uns.","public":true}}}' \
  http://127.0.0.1:3000/api/v2/tickets/1
```

## Projektstruktur

```
migrations/      SQL-Schema
src/service.rs   Ticket-/Benutzer-Logik (gemeinsam für UI und API)
src/rules.rs     Bedingungen, Aktionen, Platzhalter (+ Unit-Tests)
src/sla.rs       SLA-Auswertung
src/auth.rs      Passwörter (argon2), Sessions, Extractor
src/api/         REST-API
src/web/         Agenten-UI (askama + htmx)
templates/       HTML-Templates
static/          CSS, JS, htmx
```

## Nächste Schritte

Siehe [ROADMAP.md](ROADMAP.md) – u. a. mehrere Projekte (Mandanten), SSO, Mehrsprachigkeit,
skill-basiertes Routing, eigene/geplante Berichte und Telefonie-Warteschlangen.
