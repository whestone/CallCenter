-- E-Mail: Postausgang, Passwort-Zurücksetzen, einfache Einstellungen (Vorlagen).

CREATE TABLE email_outbox (
    id         BIGSERIAL PRIMARY KEY,
    ticket_id  BIGINT REFERENCES tickets(id) ON DELETE SET NULL,
    to_email   TEXT NOT NULL,
    subject    TEXT NOT NULL,
    body       TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed', 'skipped')),
    error      TEXT,
    attempts   INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at    TIMESTAMPTZ
);
CREATE INDEX email_outbox_pending_idx ON email_outbox (created_at) WHERE status = 'pending';

CREATE TABLE password_resets (
    token_hash TEXT PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO settings (key, value) VALUES
('email.from_name', 'Support'),
('email.signature', E'Viele Grüße\nIhr Support-Team'),
('email.reply_subject', '[#{{ticket.id}}] {{ticket.subject}}'),
('email.reply_body', E'Guten Tag {{requester.name}},\n\n{{comment}}\n\n{{signature}}\n\n—\nAntworten Sie einfach auf diese E-Mail oder sehen Sie Ihre Anfrage im Kundenportal: {{portal_link}}'),
('email.notify_body', E'Guten Tag {{requester.name}},\n\n{{comment}}\n\n{{signature}}\n\n—\nIhre Anfrage im Kundenportal: {{portal_link}}');
