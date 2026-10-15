-- Routing nach Fähigkeiten: Ein Ticket benötigt eine Fähigkeit, wenn es einen
-- ihrer Tags trägt (Tags setzen z. B. Trigger). Agenten haben Fähigkeiten.
CREATE TABLE skills (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    tags        TEXT[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_skills (
    user_id   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    skill_id  BIGINT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, skill_id)
);

-- Gespeicherte eigene Berichte (für alle Agenten sichtbar).
CREATE TABLE saved_reports (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL,
    config      JSONB NOT NULL,
    created_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
