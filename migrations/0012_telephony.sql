-- Telefonie: Anrufprotokoll und Zuordnung der Agenten zu Nebenstellen.

CREATE TABLE calls (
    id               BIGSERIAL PRIMARY KEY,
    provider         TEXT NOT NULL,                 -- sipgate | twilio | generic | simulator
    provider_call_id TEXT NOT NULL,
    direction        TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    from_number      TEXT NOT NULL DEFAULT '',
    to_number        TEXT NOT NULL DEFAULT '',
    customer_id      BIGINT REFERENCES users(id) ON DELETE SET NULL,
    agent_id         BIGINT REFERENCES users(id) ON DELETE SET NULL,
    ticket_id        BIGINT REFERENCES tickets(id) ON DELETE SET NULL,
    status           TEXT NOT NULL DEFAULT 'ringing'
                     CHECK (status IN ('ringing', 'answered', 'completed', 'missed', 'failed')),
    end_cause        TEXT,
    recording_url    TEXT,
    started_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    answered_at      TIMESTAMPTZ,
    ended_at         TIMESTAMPTZ,
    duration_seconds INT,
    UNIQUE (provider, provider_call_id)
);
CREATE INDEX calls_active_idx ON calls (started_at) WHERE status IN ('ringing', 'answered');
CREATE INDEX calls_customer_idx ON calls (customer_id);
CREATE INDEX calls_ticket_idx ON calls (ticket_id);

-- Kennung des Agenten beim Telefonie-Anbieter (z. B. sipgate "w0" oder Durchwahl).
ALTER TABLE users ADD COLUMN extension TEXT;
