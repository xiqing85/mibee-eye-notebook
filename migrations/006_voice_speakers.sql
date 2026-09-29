-- 006: voiceprint speaker profiles + hearing-record attribution.
--
-- `voice_speakers` persists the enrollment samples collected through the
-- wake-word path: `embeddings` is `count` vectors of `dim` little-endian
-- f32 values, concatenated — exactly what the voice engine's
-- `load_speakers` consumes after the web layer decodes the blob.
--
-- `hearing_records.speaker` attributes voice records to the best-matching
-- enrolled speaker ("" = unknown); sound records always stay "".

CREATE TABLE IF NOT EXISTS voice_speakers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    dim INTEGER NOT NULL,
    count INTEGER NOT NULL,
    embeddings BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

ALTER TABLE hearing_records ADD COLUMN speaker TEXT NOT NULL DEFAULT '';
