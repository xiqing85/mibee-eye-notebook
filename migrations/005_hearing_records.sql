-- Hearing records (SPEC appendix A #24): persistent text records of what
-- the audio engines recognized — sound-event classes (kind='sound') and
-- wake-word utterance transcripts (kind='voice'). FIFO-capped at 1000 rows
-- by the insert path; this table only defines the shape.

CREATE TABLE IF NOT EXISTS hearing_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('sound', 'voice')),
    text TEXT NOT NULL,
    score REAL,
    keyword TEXT NOT NULL DEFAULT '',
    timestamp_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_hearing_records_timestamp
    ON hearing_records (timestamp_ms DESC);
