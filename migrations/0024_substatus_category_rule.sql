-- Eigener Status bleibt genau so lange, wie seine Grundart zum Ticketstatus passt
-- (z. B. Kundenantwort öffnet wieder → „Warten auf Lieferant“ fällt weg; Grundart eines
-- eigenen Status geändert → Tickets ziehen mit und behalten ihn).
CREATE OR REPLACE FUNCTION tickets_sla_pause() RETURNS trigger AS $$
DECLARE
    pause JSONB;
    paused BOOLEAN;
    sub_category TEXT;
BEGIN
    IF NEW.substatus IS NOT NULL THEN
        SELECT category INTO sub_category FROM ticket_statuses WHERE key = NEW.substatus;
        IF sub_category IS DISTINCT FROM NEW.status THEN
            NEW.substatus := NULL;
        END IF;
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
