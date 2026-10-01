-- Correlated records (SPEC appendix A #30-C): what the camera SAW at
-- the moment the device HEARD something. Scene is the grounding
-- 【画面】 summary (detection labels + last VLM description) captured
-- at event time; empty for rows written before this migration.
ALTER TABLE hearing_records ADD COLUMN scene TEXT NOT NULL DEFAULT '';
