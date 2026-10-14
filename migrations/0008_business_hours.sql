-- Geschäftszeiten (ISO-Wochentag 1 = Montag … 7 = Sonntag) und Feiertage für SLAs.

CREATE TABLE business_hours (
    weekday   INT PRIMARY KEY CHECK (weekday BETWEEN 1 AND 7),
    enabled   BOOLEAN NOT NULL DEFAULT TRUE,
    opens_at  TIME NOT NULL DEFAULT '08:00',
    closes_at TIME NOT NULL DEFAULT '18:00',
    CHECK (closes_at > opens_at)
);
INSERT INTO business_hours (weekday, enabled) VALUES
    (1, TRUE), (2, TRUE), (3, TRUE), (4, TRUE), (5, TRUE), (6, FALSE), (7, FALSE);

CREATE TABLE holidays (
    day  DATE PRIMARY KEY,
    name TEXT NOT NULL
);
-- Bundesweite Feiertage Deutschland 2026/2027
INSERT INTO holidays (day, name) VALUES
    ('2026-01-01', 'Neujahr'), ('2026-04-03', 'Karfreitag'), ('2026-04-06', 'Ostermontag'),
    ('2026-05-01', 'Tag der Arbeit'), ('2026-05-14', 'Christi Himmelfahrt'), ('2026-05-25', 'Pfingstmontag'),
    ('2026-10-03', 'Tag der Deutschen Einheit'), ('2026-12-25', '1. Weihnachtstag'), ('2026-12-26', '2. Weihnachtstag'),
    ('2027-01-01', 'Neujahr'), ('2027-03-26', 'Karfreitag'), ('2027-03-29', 'Ostermontag'),
    ('2027-05-01', 'Tag der Arbeit'), ('2027-05-06', 'Christi Himmelfahrt'), ('2027-05-17', 'Pfingstmontag'),
    ('2027-10-03', 'Tag der Deutschen Einheit'), ('2027-12-25', '1. Weihnachtstag'), ('2027-12-26', '2. Weihnachtstag');
