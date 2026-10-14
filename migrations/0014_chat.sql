-- Live-Chat über ein Widget auf der Kunden-Webseite. Jeder Chat ist ein Ticket
-- (Kanal "chat"); Nachrichten sind öffentliche Kommentare.
CREATE TABLE chat_sessions (
    id                BIGSERIAL PRIMARY KEY,
    -- SHA-256 des Besucher-Tokens (das Token selbst kennt nur der Browser des Besuchers)
    token_hash        TEXT NOT NULL UNIQUE,
    ticket_id         BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    visitor_id        BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id          BIGINT REFERENCES users(id) ON DELETE SET NULL,
    status            TEXT NOT NULL DEFAULT 'waiting' CHECK (status IN ('waiting', 'active', 'ended')),
    page_url          TEXT NOT NULL DEFAULT '',
    visitor_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    visitor_typing_at TIMESTAMPTZ,
    agent_typing_at   TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at          TIMESTAMPTZ
);
CREATE INDEX chat_sessions_open_idx ON chat_sessions (status) WHERE status <> 'ended';
CREATE INDEX chat_sessions_ticket_idx ON chat_sessions (ticket_id);
