-- Agentenstatus und automatische Zuweisung (Routing) je Gruppe.

ALTER TABLE users
    ADD COLUMN availability TEXT NOT NULL DEFAULT 'offline'
        CHECK (availability IN ('online', 'away', 'offline')),
    -- Maximale Anzahl offener Tickets, die das Routing einem Agenten zuweist.
    ADD COLUMN capacity INT NOT NULL DEFAULT 15 CHECK (capacity >= 0);

ALTER TABLE agent_groups
    ADD COLUMN assignment TEXT NOT NULL DEFAULT 'manual'
        CHECK (assignment IN ('manual', 'round_robin', 'least_loaded')),
    ADD COLUMN last_assigned_user_id BIGINT REFERENCES users(id) ON DELETE SET NULL;

CREATE INDEX tickets_unassigned_idx ON tickets (group_id, created_at)
    WHERE assignee_id IS NULL AND status IN ('new', 'open');
