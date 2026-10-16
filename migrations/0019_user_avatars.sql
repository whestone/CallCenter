-- Profilfotos (eigene Tabelle, damit normale Benutzerabfragen keine Bilddaten laden).
CREATE TABLE user_avatars (
    user_id      BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    data         BYTEA NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
