-- Kundenzufriedenheit (CSAT) und Dateianhänge.

ALTER TABLE tickets
    ADD COLUMN satisfaction_score TEXT CHECK (satisfaction_score IN ('good', 'bad')),
    ADD COLUMN satisfaction_comment TEXT,
    ADD COLUMN satisfaction_at TIMESTAMPTZ;

CREATE TABLE attachments (
    id           BIGSERIAL PRIMARY KEY,
    ticket_id    BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    comment_id   BIGINT REFERENCES ticket_comments(id) ON DELETE CASCADE,
    filename     TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size         BIGINT NOT NULL,
    data         BYTEA NOT NULL,
    uploaded_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX attachments_ticket_idx ON attachments (ticket_id);
CREATE INDEX attachments_comment_idx ON attachments (comment_id);
