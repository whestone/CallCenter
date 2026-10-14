-- Frei verknüpfte Tickets („verwandt“), zusätzlich zu Problem ↔ Vorfall und Folgetickets.
-- Ein Paar wird nur einmal gespeichert (kleinere ID zuerst).
CREATE TABLE ticket_links (
    a_id        BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    b_id        BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    created_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (a_id, b_id),
    CHECK (a_id < b_id)
);
CREATE INDEX ticket_links_b_idx ON ticket_links (b_id);
