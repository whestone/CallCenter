-- Eigene Ticketfelder; Werte liegen in tickets.custom_fields unter dem Schlüssel `key`.

CREATE TABLE ticket_fields (
    id                BIGSERIAL PRIMARY KEY,
    key               TEXT NOT NULL UNIQUE CHECK (key ~ '^[a-z][a-z0-9_]{0,39}$'),
    label             TEXT NOT NULL,
    field_type        TEXT NOT NULL CHECK (field_type IN ('text', 'number', 'dropdown', 'checkbox', 'date')),
    options           TEXT[] NOT NULL DEFAULT '{}',
    required_on_solve BOOLEAN NOT NULL DEFAULT FALSE,
    position          INT NOT NULL DEFAULT 0,
    active            BOOLEAN NOT NULL DEFAULT TRUE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
