-- Zusammenarbeit im Team: verknüpfte Tickets, CC, Benachrichtigungen,
-- SLA-Eskalationen, Zeiterfassung und persönliche Makros.

-- Problem ↔ Vorfälle (wie Zendesk problem_id) und Folgetickets zu geschlossenen Tickets.
ALTER TABLE tickets
    ADD COLUMN problem_id BIGINT REFERENCES tickets(id) ON DELETE SET NULL,
    ADD COLUMN followup_source_id BIGINT REFERENCES tickets(id) ON DELETE SET NULL,
    -- Kunden in CC erhalten öffentliche Antworten ebenfalls per E-Mail.
    ADD COLUMN cc_ids BIGINT[] NOT NULL DEFAULT '{}',
    -- Merker, für welches SLA-Ziel (Metrik + Fälligkeit) schon gewarnt bzw. eskaliert wurde.
    ADD COLUMN sla_warn_key TEXT,
    ADD COLUMN sla_breach_key TEXT;
CREATE INDEX tickets_problem_idx ON tickets (problem_id) WHERE problem_id IS NOT NULL;

-- Benachrichtigungen für Agenten (Glocke in der Kopfzeile).
CREATE TABLE notifications (
    id          BIGSERIAL PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    ticket_id   BIGINT REFERENCES tickets(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    read_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC);
CREATE INDEX notifications_unread_idx ON notifications (user_id) WHERE read_at IS NULL;

-- Zeiterfassung pro Ticket.
CREATE TABLE time_entries (
    id          BIGSERIAL PRIMARY KEY,
    ticket_id   BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    user_id     BIGINT REFERENCES users(id) ON DELETE SET NULL,
    seconds     INT NOT NULL CHECK (seconds > 0),
    note        TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX time_entries_ticket_idx ON time_entries (ticket_id);
CREATE INDEX time_entries_user_idx ON time_entries (user_id, created_at);

-- Persönliche Makros: owner_id gesetzt = nur für diesen Agenten sichtbar.
ALTER TABLE macros ADD COLUMN owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;
