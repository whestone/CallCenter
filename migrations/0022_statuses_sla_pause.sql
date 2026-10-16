-- Status konfigurierbar: Bezeichnung, Farbe, Reihenfolge, aktiv; erlaubte Übergänge.
-- Die Schlüssel (new, open, pending, hold, solved, closed) bleiben fest, weil Regeln,
-- SLA, Berichte und Portal an ihnen hängen.
CREATE TABLE ticket_statuses (
    key      TEXT PRIMARY KEY,
    label    TEXT NOT NULL,
    color    TEXT NOT NULL,
    position INT NOT NULL,
    active   BOOLEAN NOT NULL DEFAULT TRUE
);
INSERT INTO ticket_statuses (key, label, color, position) VALUES
    ('new', 'Neu', '#f59e0b', 1),
    ('open', 'Offen', '#2563eb', 2),
    ('pending', 'Wartend', '#7c5cff', 3),
    ('hold', 'Angehalten', '#64748b', 4),
    ('solved', 'Gelöst', '#059669', 5),
    ('closed', 'Geschlossen', '#94a3b8', 6);

CREATE TABLE status_transitions (
    from_key TEXT NOT NULL REFERENCES ticket_statuses(key) ON DELETE CASCADE,
    to_key   TEXT NOT NULL REFERENCES ticket_statuses(key) ON DELETE CASCADE,
    PRIMARY KEY (from_key, to_key)
);
-- Bisheriges Verhalten: alles erlaubt, außer zurück auf „Neu“, weg von „Geschlossen“
-- und „Geschlossen“ nur aus „Gelöst“.
INSERT INTO status_transitions (from_key, to_key)
SELECT a.key, b.key FROM ticket_statuses a, ticket_statuses b
WHERE a.key <> b.key AND b.key <> 'new' AND a.key <> 'closed' AND (b.key <> 'closed' OR a.key = 'solved');

-- SLA-Pause: in welchen Status die Uhr einer Richtlinie steht (Standard: Wartend).
ALTER TABLE sla_policies ADD COLUMN pause_statuses JSONB NOT NULL DEFAULT '["pending"]';
ALTER TABLE tickets ADD COLUMN sla_paused_seconds BIGINT NOT NULL DEFAULT 0;
ALTER TABLE tickets ADD COLUMN sla_paused_since TIMESTAMPTZ;

-- Pausenzeit bei jedem Statuswechsel mitführen – unabhängig davon, wo der Wechsel
-- herkommt (Oberfläche, E-Mail, Regeln, API).
CREATE FUNCTION tickets_sla_pause() RETURNS trigger AS $$
DECLARE
    pause JSONB;
    paused BOOLEAN;
BEGIN
    SELECT pause_statuses INTO pause FROM sla_policies WHERE id = NEW.sla_policy_id;
    paused := coalesce(pause, '[]'::jsonb) ? NEW.status;
    IF TG_OP = 'INSERT' THEN
        NEW.sla_paused_since := CASE WHEN paused THEN now() ELSE NULL END;
        RETURN NEW;
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status OR NEW.sla_policy_id IS DISTINCT FROM OLD.sla_policy_id THEN
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

CREATE TRIGGER tickets_sla_pause BEFORE INSERT OR UPDATE ON tickets
FOR EACH ROW EXECUTE FUNCTION tickets_sla_pause();

-- Tickets, die gerade in einem pausierenden Status stehen, ab jetzt als pausiert führen.
UPDATE tickets t SET sla_paused_since = now()
FROM sla_policies p
WHERE p.id = t.sla_policy_id AND p.pause_statuses ? t.status AND t.sla_paused_since IS NULL;
