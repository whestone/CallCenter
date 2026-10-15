-- Persönliche Oberflächen-Einstellungen (z. B. sichtbare Filter und Spalten der Ticketlisten).
CREATE TABLE user_prefs (
    user_id     BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    ticket_list JSONB NOT NULL DEFAULT '{}',
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
