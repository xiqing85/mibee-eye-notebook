-- Protocol configurations (ONVIF, GB28181, RTMP push).
--
-- Each row stores one protocol's full config as a JSON blob, keyed by
-- protocol name. Values are upserted on update.
--
-- Initial seeding from config.toml happens in main.rs on first run
-- (when this table is empty after migration).

CREATE TABLE IF NOT EXISTS protocol_configs (
    key TEXT PRIMARY KEY,          -- 'onvif' | 'gb28181' | 'rtmp_push'
    value TEXT NOT NULL,           -- JSON blob with the protocol's config
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
