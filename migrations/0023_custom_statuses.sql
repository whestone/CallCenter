-- Eigene Status: jeder Status hat eine Grundart (new, open, pending, hold, solved, closed).
-- Das Ticket behält die Grundart in `status` (Regeln, SLA, Berichte, Portal bleiben
-- verlässlich) und trägt einen eigenen Status zusätzlich in `substatus`.
ALTER TABLE ticket_statuses ADD COLUMN category TEXT;
UPDATE ticket_statuses SET category = key;
ALTER TABLE ticket_statuses ALTER COLUMN category SET NOT NULL;
ALTER TABLE ticket_statuses ADD COLUMN custom BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ticket_statuses ADD CONSTRAINT ticket_statuses_category_check
    CHECK (category IN ('new', 'open', 'pending', 'hold', 'solved', 'closed'));

ALTER TABLE tickets ADD COLUMN substatus TEXT REFERENCES ticket_statuses(key) ON DELETE SET NULL;

-- SLA-Pause richtet sich nach dem angezeigten Status (eigener Status, sonst Grundart).
-- Wechselt die Grundart ohne neuen eigenen Status (z. B. Kundenantwort öffnet wieder),
-- fällt der eigene Status weg.
CREATE OR REPLACE FUNCTION tickets_sla_pause() RETURNS trigger AS $$
DECLARE
    pause JSONB;
    paused BOOLEAN;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.status IS DISTINCT FROM OLD.status AND NEW.substatus IS NOT DISTINCT FROM OLD.substatus THEN
        NEW.substatus := NULL;
    END IF;
    SELECT pause_statuses INTO pause FROM sla_policies WHERE id = NEW.sla_policy_id;
    paused := coalesce(pause, '[]'::jsonb) ? coalesce(NEW.substatus, NEW.status);
    IF TG_OP = 'INSERT' THEN
        NEW.sla_paused_since := CASE WHEN paused THEN now() ELSE NULL END;
        RETURN NEW;
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status OR NEW.substatus IS DISTINCT FROM OLD.substatus
       OR NEW.sla_policy_id IS DISTINCT FROM OLD.sla_policy_id THEN
        IF OLD.sla_paused_since IS NOT NULL AND NOT paused THEN
            NEW.sla_paused_seconds := OLD.sla_paused_seconds + greatest(0, extract(epoch FROM now() - OLD.sla_paused_since))::bigint;
            NEW.sla_paused_since := NULL;
        ELSIF OLD.sla_paused_since IS NULL AND paused THEN
            NEW.sla_paused_since := now();
        END IF;
    END IF;
    RETURN NEW;
END
$$ LANGUAGE plpgsql;
