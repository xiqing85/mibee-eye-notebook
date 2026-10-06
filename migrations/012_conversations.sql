-- Conversation records (SPEC §3.4): the human-readable log of every
-- dialogue turn — heard/input text, internal "thinking" entries (one
-- per internal model call or routing decision, stored as JSON), and the
-- AI reply with the engine that produced it. FIFO-capped at 1000 rows
-- by the insert path; this table only defines the shape.

CREATE TABLE IF NOT EXISTS conversation_turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('voice', 'http')),
    started_ms INTEGER NOT NULL,
    user_text TEXT,
    thinking_json TEXT NOT NULL DEFAULT '[]',
    reply_text TEXT,
    engine TEXT
);

CREATE INDEX IF NOT EXISTS idx_conversation_turns_id ON conversation_turns(id DESC);
