-- Ausgehende Webhooks mit Zustellprotokoll.

CREATE TABLE webhooks (
    id         BIGSERIAL PRIMARY KEY,
    name       TEXT NOT NULL,
    url        TEXT NOT NULL CHECK (url ~ '^https?://'),
    secret     TEXT NOT NULL,
    events     TEXT[] NOT NULL DEFAULT '{ticket.created,ticket.updated,ticket.solved}',
    active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webhook_deliveries (
    id            BIGSERIAL PRIMARY KEY,
    webhook_id    BIGINT NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    event         TEXT NOT NULL,
    payload       JSONB NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    response_code INT,
    error         TEXT,
    attempts      INT NOT NULL DEFAULT 0,
    next_try_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at       TIMESTAMPTZ
);
CREATE INDEX webhook_deliveries_pending_idx ON webhook_deliveries (next_try_at) WHERE status = 'pending';
