-- Online AI configuration (SPEC §4.10, capability cloud_ai).
-- A dedicated table, deliberately NOT the settings bag: the API key must
-- never surface through GET /api/settings or /api/config — readers see
-- only the derived api_key_set boolean via GET /api/cloud.
CREATE TABLE IF NOT EXISTS cloud_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    provider TEXT NOT NULL DEFAULT 'off',
    api_key TEXT NOT NULL DEFAULT '',
    chat_model TEXT NOT NULL DEFAULT '',
    vision_model TEXT NOT NULL DEFAULT '',
    fallback_local INTEGER NOT NULL DEFAULT 1,
    timeout_secs INTEGER NOT NULL DEFAULT 60
);

INSERT OR IGNORE INTO cloud_config (id) VALUES (1);
