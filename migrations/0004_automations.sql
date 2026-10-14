-- Zeitgesteuerte Automationen (wie Zendesk "Automations") und Systembenutzer.

CREATE TABLE automations (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    conditions  JSONB NOT NULL DEFAULT '{"all": [], "any": []}',
    actions     JSONB NOT NULL DEFAULT '[]',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Eine Automation feuert je Ticketstand höchstens einmal; ändert sich das Ticket
-- danach (updated_at > ran_at), darf sie erneut greifen.
CREATE TABLE automation_runs (
    automation_id BIGINT NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
    ticket_id     BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    ran_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (automation_id, ticket_id)
);

-- Technischer Benutzer, unter dem Automationen Tickets ändern (nicht anmeldbar).
INSERT INTO users (name, email, role, active)
VALUES ('Automatisierung', 'system@automation.invalid', 'admin', FALSE)
ON CONFLICT DO NOTHING;

-- Ersetzt das bisher fest eingebaute Schließen gelöster Tickets.
INSERT INTO automations (title, description, position, conditions, actions) VALUES
('Gelöste Tickets nach 4 Tagen schließen', 'Standardverhalten wie bei Zendesk', 0,
 '{"all": [{"field": "status", "operator": "is", "value": "solved"},
           {"field": "hours_since_solved", "operator": "greater_than", "value": 96}], "any": []}',
 '[{"field": "status", "value": "closed"}]'),
('Erinnerung nach 3 Tagen „Wartend“', 'Kunde wird einmalig an offene Rückfrage erinnert', 1,
 '{"all": [{"field": "status", "operator": "is", "value": "pending"},
           {"field": "hours_since_updated", "operator": "greater_than", "value": 72}], "any": []}',
 '[{"field": "comment_value", "value": "Guten Tag,\n\nwir warten noch auf Ihre Rückmeldung zu Ihrer Anfrage #{{ticket.id}} „{{ticket.subject}}“. Ohne Antwort schließen wir die Anfrage in einigen Tagen automatisch.\n\nIhr Support-Team"},
   {"field": "add_tags", "value": "erinnerung_gesendet"}]');
