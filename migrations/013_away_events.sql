-- Away mode event records (SPEC v1 §3.6, notebook dialect appendix A #44).
-- One row per accepted rising edge while armed: person visitors (with
-- face match, VLM description and the visitor's spoken answer) or
-- configured activity labels. Snapshots live as files under
-- [away] snapshot_dir; the column stores the server-generated file
-- name only (never a client-supplied path). FIFO-pruned by the insert
-- path, which also returns the pruned snapshot names for unlinking.
CREATE TABLE IF NOT EXISTS away_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('person', 'activity')),
    started_ms INTEGER NOT NULL,
    labels TEXT NOT NULL DEFAULT '',
    face_name TEXT,
    description TEXT,
    visitor_reply TEXT,
    snapshot TEXT,
    state TEXT NOT NULL DEFAULT 'recorded'
);
CREATE INDEX IF NOT EXISTS idx_away_events_id ON away_events(id DESC);
