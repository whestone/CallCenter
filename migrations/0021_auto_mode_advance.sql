-- Auto-Modus: nach welchen Status es zur nächsten Aufgabe weitergeht (NULL = nur „gelöst“).
ALTER TABLE agent_auto ADD COLUMN advance_on JSONB;
