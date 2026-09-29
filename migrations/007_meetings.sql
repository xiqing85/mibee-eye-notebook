-- Meeting mode (SPEC appendix A #27): one row per recording session, one
-- row per diarized+transcribed segment.
CREATE TABLE IF NOT EXISTS meeting_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    duration_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'recording',
    num_speakers INTEGER,
    num_segments INTEGER,
    audio_path TEXT NOT NULL DEFAULT '',
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS meeting_segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id INTEGER NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    speaker_index INTEGER NOT NULL,
    speaker TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_meeting_segments_meeting
    ON meeting_segments(meeting_id);
