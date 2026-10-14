-- Zwei-Faktor-Anmeldung (TOTP) für Agenten.
ALTER TABLE users
    ADD COLUMN totp_secret TEXT,
    ADD COLUMN totp_pending_secret TEXT;

-- Zwischenschritt nach korrektem Passwort, bis der Code bestätigt ist (5 Minuten gültig).
CREATE TABLE login_challenges (
    token_hash TEXT PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    attempts   INT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL
);
