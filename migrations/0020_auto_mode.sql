-- Auto-Modus: Agent bekommt automatisch die nächste Aufgabe.
-- sources: geordnete Liste der Quellen, z. B. ["chat", "view:3", "view:1"].
CREATE TABLE agent_auto (
    user_id    BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    enabled    BOOLEAN NOT NULL DEFAULT false,
    sources    JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
