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

### E-Mail (Admin → E-Mail)
- Kunden erhalten öffentliche Antworten und Trigger-Benachrichtigungen per Mail; Betreff mit `[#123]`
- Versand per SMTP (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD`, `SMTP_FROM`, `SMTP_SECURITY`);
  ohne Konfiguration landen Mails im Postausgang mit Status „nicht gesendet“
- Eingang: `POST /api/v2/inbound/email?secret=…` mit JSON `{from, from_name, subject, text}`
  (z. B. von Mailgun/Postmark weitergeleitet). Antworten mit `[#123]` vom Anfragenden werden dem Ticket
  zugeordnet, Zitate abgeschnitten; alles andere wird ein neues Ticket. Secret: `INBOUND_EMAIL_SECRET`
- „Passwort vergessen?“ für Agenten und Kunden (Einmal-Link, 1 Stunde gültig); `PUBLIC_URL` für Links

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
`drucker status:open priority:urgent tag:vip assignee:me requester:max@acme.example group:support`

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

- Telefonie-Anbindung (Twilio/sipgate-Webhooks → Ticket + Call-Record, Screen-Pop beim Agenten)
- E-Mail-Kanal (IMAP/SMTP) und echte Benachrichtigungen statt Log
- Business-Hours für SLAs, Satisfaction-Rating, Reporting
- API-Tokens statt Passwort-Basic-Auth, CSRF-Token für Formulare
- View-Bedingungen in SQL übersetzen (aktuell In-Memory-Auswertung über max. 20 000 Tickets)
