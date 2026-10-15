-- Persönliche Einstellungen für alle übrigen Listen (Kunden, Organisationen, Chats, Team …),
-- je Liste ein Eintrag: {"users": {...}, "orgs": {...}}.
ALTER TABLE user_prefs ADD COLUMN lists JSONB NOT NULL DEFAULT '{}';
