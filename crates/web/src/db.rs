use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A camera row as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraRow {
    pub id: String,
    pub name: String,
    pub camera_type: String,
    pub config: serde_json::Value,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Initialize the database at `path`:
/// 1. Open SQLite with WAL mode and busy timeout
/// 2. Ensure the schema_version tracking table exists
/// 3. Apply any pending migrations
pub fn init_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).context("Failed to open SQLite database")?;

    // Enable WAL mode for better concurrent read performance
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .context("Failed to set WAL mode")?;

    // Set busy timeout (5 seconds) so concurrent access doesn't fail immediately
    conn.execute_batch("PRAGMA busy_timeout=5000;")
        .context("Failed to set busy timeout")?;

    ensure_migrations(&conn).context("Failed to run database migrations")?;

    Ok(conn)
}

/// Ensure the schema_version table exists, read current version, apply pending
/// migrations, and update the version.
fn ensure_migrations(conn: &Connection) -> Result<()> {
    // Create version tracking table if it doesn't exist
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
        .context("Failed to create schema_version table")?;

    // Get current version (0 if none)
    let current_version: i32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Discover migration files: look for `{migrations_dir}/NNN_*.sql`
    let migrations_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("migrations"))
        .unwrap_or_else(|| Path::new("migrations").to_path_buf());

    if !migrations_dir.exists() {
        // No migrations directory — nothing to do
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&migrations_dir)
        .context("Failed to read migrations directory")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "sql")
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Parse numeric prefix: "001_initial.sql" -> 1
            let num: i32 = name.split('_').next().and_then(|s| s.parse().ok())?;
            Some((num, e.path()))
        })
        .collect();

    entries.sort_by_key(|(num, _)| *num);

    // Apply each migration that hasn't been applied yet
    for (num, path) in &entries {
        if *num <= current_version {
            continue;
        }

        let sql = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read migration file: {}", path.display()))?;

        conn.execute_batch(&sql)
            .with_context(|| format!("Failed to apply migration {}", num))?;

        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![num],
        )
        .context("Failed to update schema_version")?;

        tracing::info!(migration = num, "Applied database migration");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Camera CRUD
// ---------------------------------------------------------------------------

pub fn create_camera(conn: &Connection, camera: &CameraRow) -> Result<()> {
    let config_str = serde_json::to_string(&camera.config).context("Failed to serialize config")?;

    conn.execute(
        "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            camera.id,
            camera.name,
            camera.camera_type,
            config_str,
            camera.status,
            camera.created_at,
            camera.updated_at,
        ],
    )
    .context("Failed to create camera")?;

    Ok(())
}

pub fn get_camera(conn: &Connection, id: &str) -> Result<Option<CameraRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, camera_type, config, status, created_at, updated_at
             FROM cameras WHERE id = ?1",
        )
        .context("Failed to prepare get_camera statement")?;

    let mut rows = stmt.query(rusqlite::params![id])?;
    match rows.next()? {
        Some(row) => {
            let config_str: String = row.get(3)?;
            let config: serde_json::Value =
                serde_json::from_str(&config_str).context("Failed to parse camera config JSON")?;
            Ok(Some(CameraRow {
                id: row.get(0)?,
                name: row.get(1)?,
                camera_type: row.get(2)?,
                config,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            }))
        }
        None => Ok(None),
    }
}

pub fn list_cameras(conn: &Connection) -> Result<Vec<CameraRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, camera_type, config, status, created_at, updated_at
             FROM cameras ORDER BY created_at DESC",
        )
        .context("Failed to prepare list_cameras statement")?;

    let rows = stmt
        .query_map([], |row| {
            let config_str: String = row.get(3)?;
            let config: serde_json::Value = serde_json::from_str(&config_str)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok(CameraRow {
                id: row.get(0)?,
                name: row.get(1)?,
                camera_type: row.get(2)?,
                config,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })
        .context("Failed to query list_cameras")?;

    let mut cameras = Vec::new();
    for row in rows {
        cameras.push(row.context("Failed to read camera row")?);
    }
    Ok(cameras)
}

/// Auto-discover physically attached video devices and create camera entries
/// for any that don't already exist in the database.
///
/// This runs on startup so the user sees their webcam in the dashboard
/// immediately without manual configuration.
pub fn auto_discover_cameras(conn: &Connection) -> Result<usize> {
    let devices = match capture::video::enumerate_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "failed to enumerate video devices during auto-discovery");
            return Ok(0);
        }
    };

    let existing = list_cameras(conn)?;
    let existing_indices: std::collections::HashSet<i64> = existing
        .iter()
        .filter(|c| c.camera_type == "usb")
        .filter_map(|c| c.config.get("device_index").and_then(|v| v.as_i64()))
        .collect();

    let mut discovered = 0;
    for dev in &devices {
        let idx = dev.index as i64;
        if existing_indices.contains(&idx) {
            continue; // already in DB
        }

        // Skip metadata-only device nodes (UVC cameras expose multiple /dev/videoN,
        // only one has actual capture formats).
        if dev.formats.is_empty() {
            tracing::debug!(index = dev.index, "skipping device with no formats (likely metadata node)");
            continue;
        }

        let name = if dev.name.is_empty() {
            format!("USB Camera {}", dev.index)
        } else {
            dev.name.clone()
        };

        let now = chrono_epoch_secs();
        let camera = CameraRow {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            camera_type: "usb".to_string(),
            config: serde_json::json!({"device_index": dev.index}),
            status: "stopped".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };

        match create_camera(conn, &camera) {
            Ok(()) => {
                tracing::info!(
                    index = dev.index,
                    name = %camera.name,
                    "auto-discovered camera"
                );
                discovered += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, index = dev.index, "failed to auto-discover camera");
            }
        }
    }

    Ok(discovered)
}

fn chrono_epoch_secs() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

pub fn update_camera(conn: &Connection, camera: &CameraRow) -> Result<()> {
    let config_str = serde_json::to_string(&camera.config).context("Failed to serialize config")?;

    let affected = conn
        .execute(
            "UPDATE cameras SET name = ?1, camera_type = ?2, config = ?3,
                    status = ?4, updated_at = ?5
             WHERE id = ?6",
            rusqlite::params![
                camera.name,
                camera.camera_type,
                config_str,
                camera.status,
                camera.updated_at,
                camera.id,
            ],
        )
        .context("Failed to update camera")?;

    if affected == 0 {
        anyhow::bail!("Camera with id '{}' not found", camera.id);
    }

    Ok(())
}

pub fn delete_camera(conn: &Connection, id: &str) -> Result<()> {
    let affected = conn
        .execute("DELETE FROM cameras WHERE id = ?1", rusqlite::params![id])
        .context("Failed to delete camera")?;

    if affected == 0 {
        anyhow::bail!("Camera with id '{}' not found", id);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Settings CRUD
// ---------------------------------------------------------------------------

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value FROM settings WHERE key = ?1")
        .context("Failed to prepare get_setting statement")?;

    let mut rows = stmt.query(rusqlite::params![key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![key, value],
    )
    .context("Failed to set setting")?;

    Ok(())
}

/// List all settings as a vector of (key, value) pairs.
pub fn list_settings(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM settings ORDER BY key")
        .context("Failed to prepare list_settings statement")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("Failed to query list_settings")?;
    let mut settings = Vec::new();
    for row in rows {
        settings.push(row.context("Failed to read setting row")?);
    }
    Ok(settings)
}

// ---------------------------------------------------------------------------
// Protocol configs (onvif / gb28181 / rtmp_push)
// ---------------------------------------------------------------------------

/// Retrieve one protocol's config as a JSON Value, or None if not stored.
pub fn get_protocol_config(conn: &Connection, key: &str) -> Result<Option<serde_json::Value>> {
    let mut stmt = conn
        .prepare("SELECT value FROM protocol_configs WHERE key = ?1")
        .context("Failed to prepare get_protocol_config statement")?;
    let mut rows = stmt.query(rusqlite::params![key])?;
    match rows.next()? {
        Some(row) => {
            let s: String = row.get(0)?;
            let v: serde_json::Value =
                serde_json::from_str(&s).context("Failed to parse protocol config JSON")?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// Upsert one protocol config (replaces existing value, updates timestamp).
pub fn set_protocol_config(conn: &Connection, key: &str, value: &serde_json::Value) -> Result<()> {
    let s = serde_json::to_string(value).context("Failed to serialize protocol config")?;
    let sql = "INSERT INTO protocol_configs (key, value, updated_at) VALUES (?1, ?2, datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at";
    conn.execute(sql, rusqlite::params![key, s])
        .context("Failed to upsert protocol config")?;
    Ok(())
}

/// Return all stored protocol configs as (key, value) pairs, ordered by key.
pub fn list_protocol_configs(conn: &Connection) -> Result<Vec<(String, serde_json::Value)>> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM protocol_configs ORDER BY key")
        .context("Failed to prepare list_protocol_configs statement")?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let value_str: String = row.get(1)?;
        let value: serde_json::Value = serde_json::from_str(&value_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok((key, value))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("Failed to read protocol_configs row")?);
    }
    Ok(out)
}

/// Return true if the protocol_configs table has zero rows (used at startup
/// to decide whether to seed from config.toml).
pub fn protocol_configs_is_empty(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM protocol_configs", [], |row| {
        row.get(0)
    })?;
    Ok(count == 0)
}

// ---------------------------------------------------------------------------
// Stream Session logging
// ---------------------------------------------------------------------------

/// Insert a new stream session row for audit purposes.
/// The session starts now with default zero counters.
pub fn insert_stream_session(conn: &Connection, session_id: &str, camera_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO stream_sessions (id, camera_id) VALUES (?1, ?2)",
        rusqlite::params![session_id, camera_id],
    )
    .context("Failed to insert stream session")?;
    Ok(())
}

/// Finalize (end) the most recent open session for a camera.
/// Updates stats if provided, otherwise records 0s.
/// Returns the number of rows updated (0 or 1).
pub fn finalize_stream_session(
    conn: &Connection,
    camera_id: &str,
    bytes_received: i64,
    frames_received: i64,
    error_count: i64,
) -> Result<usize> {
    let affected = conn
        .execute(
            "UPDATE stream_sessions
             SET ended_at = datetime('now'),
                 bytes_received = ?1,
                 frames_received = ?2,
                 error_count = ?3
             WHERE id = (
                 SELECT id FROM stream_sessions
                 WHERE camera_id = ?4 AND ended_at IS NULL
                 ORDER BY started_at DESC LIMIT 1
             )",
            rusqlite::params![bytes_received, frames_received, error_count, camera_id],
        )
        .context("Failed to finalize stream session")?;
    Ok(affected)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an in-memory database for testing.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Run migrations manually using a simplified path (since CARGO_MANIFEST_DIR
        // may not resolve in tests, we just execute the SQL directly).
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .unwrap();
        // Apply 001_initial inline
        conn.execute_batch(include_str!("../../../migrations/001_initial.sql"))
            .unwrap();
        conn.execute("INSERT INTO schema_version (version) VALUES (1)", [])
            .unwrap();
        conn
    }

    #[test]
    fn test_migration_tables_exist() {
        let conn = test_db();

        // Verify all tables were created
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            tables.contains(&"cameras".to_string()),
            "cameras table should exist"
        );
        assert!(
            tables.contains(&"settings".to_string()),
            "settings table should exist"
        );
        assert!(
            tables.contains(&"stream_sessions".to_string()),
            "stream_sessions table should exist"
        );
    }

    #[test]
    fn test_camera_crud_roundtrip() {
        let conn = test_db();

        let camera = CameraRow {
            id: "cam-001".to_string(),
            name: "Front Door".to_string(),
            camera_type: "rtsp".to_string(),
            config: serde_json::json!({"url": "rtsp://192.168.1.100:554/stream1"}),
            status: "stopped".to_string(),
            created_at: "2025-01-01T00:00:00".to_string(),
            updated_at: "2025-01-01T00:00:00".to_string(),
        };

        // Create
        create_camera(&conn, &camera).unwrap();

        // Read
        let fetched = get_camera(&conn, "cam-001")
            .unwrap()
            .expect("Camera should exist");
        assert_eq!(fetched.name, "Front Door");
        assert_eq!(fetched.camera_type, "rtsp");
        assert_eq!(fetched.config["url"], "rtsp://192.168.1.100:554/stream1");

        // Update
        let mut updated = camera.clone();
        updated.name = "Back Door".to_string();
        updated.status = "running".to_string();
        updated.updated_at = "2025-01-02T00:00:00".to_string();
        update_camera(&conn, &updated).unwrap();

        let fetched = get_camera(&conn, "cam-001")
            .unwrap()
            .expect("Camera should exist after update");
        assert_eq!(fetched.name, "Back Door");
        assert_eq!(fetched.status, "running");

        // List
        let all = list_cameras(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "cam-001");

        // Delete
        delete_camera(&conn, "cam-001").unwrap();
        let fetched = get_camera(&conn, "cam-001").unwrap();
        assert!(fetched.is_none(), "Camera should be deleted");

        // List empty
        let all = list_cameras(&conn).unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_settings_get_set() {
        let conn = test_db();

        // Get non-existent key
        let val = get_setting(&conn, "nonexistent").unwrap();
        assert!(val.is_none());

        // Set and get
        set_setting(&conn, "theme", "dark").unwrap();
        let val = get_setting(&conn, "theme").unwrap();
        assert_eq!(val, Some("dark".to_string()));

        // Update
        set_setting(&conn, "theme", "light").unwrap();
        let val = get_setting(&conn, "theme").unwrap();
        assert_eq!(val, Some("light".to_string()));

        // Multiple settings
        set_setting(&conn, "language", "zh-CN").unwrap();
        let val = get_setting(&conn, "language").unwrap();
        assert_eq!(val, Some("zh-CN".to_string()));

        // Original still intact
        let val = get_setting(&conn, "theme").unwrap();
        assert_eq!(val, Some("light".to_string()));
    }

    #[test]
    fn test_delete_nonexistent_camera_fails() {
        let conn = test_db();
        let result = delete_camera(&conn, "no-such-camera");
        assert!(result.is_err(), "Deleting nonexistent camera should fail");
    }

    #[test]
    fn test_update_nonexistent_camera_fails() {
        let conn = test_db();
        let camera = CameraRow {
            id: "no-such".to_string(),
            name: "Ghost".to_string(),
            camera_type: "rtsp".to_string(),
            config: serde_json::Value::Object(Default::default()),
            status: "stopped".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        };
        let result = update_camera(&conn, &camera);
        assert!(result.is_err(), "Updating nonexistent camera should fail");
    }

    #[test]
    fn test_cameras_have_type_index() {
        let conn = test_db();
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='cameras'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            indexes.contains(&"idx_cameras_type".to_string()),
            "idx_cameras_type index should exist"
        );
    }

    // -----------------------------------------------------------------------
    // Stream Session tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_insert_and_finalize_stream_session() {
        let conn = test_db();

        // Create a camera first (FK constraint)
        conn.execute(
            concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            ),
            rusqlite::params!["cam-001", "Front Door", "usb", "{}", "stopped", "now", "now"],
        )
        .unwrap();

        // Insert a stream session
        insert_stream_session(&conn, "sess-001", "cam-001").unwrap();

        // Verify the row exists with ended_at IS NULL
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stream_sessions
                 WHERE camera_id = ?1 AND ended_at IS NULL",
                rusqlite::params!["cam-001"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "should have one open session");

        // Finalize the session with stats
        let updated = finalize_stream_session(&conn, "cam-001", 1024, 30, 0).unwrap();
        assert_eq!(updated, 1, "should update exactly one row");

        // Verify the row now has ended_at NOT NULL and stats recorded
        let (ended_count, bytes, frames, errors): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), bytes_received, frames_received, error_count
                 FROM stream_sessions
                 WHERE camera_id = ?1 AND ended_at IS NOT NULL",
                rusqlite::params!["cam-001"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(ended_count, 1, "should have one ended session");
        assert_eq!(bytes, 1024);
        assert_eq!(frames, 30);
        assert_eq!(errors, 0);
    }

    #[test]
    fn test_insert_stream_session_uses_defaults() {
        let conn = test_db();

        // Create a camera first (FK constraint)
        conn.execute_batch(concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES ('cam-002', 'Test', 'usb', '{}', 'stopped', datetime('now'), datetime('now'));",
        ))
        .unwrap();

        insert_stream_session(&conn, "sess-002", "cam-002").unwrap();

        // Verify default values for numeric columns
        let (bytes, frames, errors): (i64, i64, i64) = conn
            .query_row(
                "SELECT bytes_received, frames_received, error_count
                 FROM stream_sessions WHERE id = ?1",
                rusqlite::params!["sess-002"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(bytes, 0, "bytes_received should default to 0");
        assert_eq!(frames, 0, "frames_received should default to 0");
        assert_eq!(errors, 0, "error_count should default to 0");
    }

    #[test]
    fn test_finalize_only_latest_open_session() {
        let conn = test_db();

        // Create a camera first (FK constraint)
        conn.execute(
            concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now'))",
            ),
            rusqlite::params!["cam-001", "Front Door", "usb", "{}", "stopped"],
        )
        .unwrap();

        // Insert two open sessions with explicit timestamps to ensure ordering
        conn.execute(
            concat!(
                "INSERT INTO stream_sessions (id, camera_id, started_at) ",
                "VALUES (?1, ?2, ?3)",
            ),
            rusqlite::params!["sess-a", "cam-001", "2025-01-01T00:00:00"],
        )
        .unwrap();
        conn.execute(
            concat!(
                "INSERT INTO stream_sessions (id, camera_id, started_at) ",
                "VALUES (?1, ?2, ?3)",
            ),
            rusqlite::params!["sess-b", "cam-001", "2025-01-02T00:00:00"],
        )
        .unwrap();

        // Finalize — should only affect the latest (sess-b)
        let updated = finalize_stream_session(&conn, "cam-001", 500, 10, 1).unwrap();
        assert_eq!(updated, 1, "should update exactly one row");

        // sess-a should still be open
        let open_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stream_sessions
                 WHERE camera_id = ?1 AND ended_at IS NULL",
                rusqlite::params!["cam-001"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(open_count, 1, "sess-a should still be open");

        // The finalized one should have the stats
        let (sess_b_ended, bytes, frames, errors): (bool, i64, i64, i64) = conn
            .query_row(
                "SELECT ended_at IS NOT NULL, bytes_received, frames_received, error_count
                 FROM stream_sessions WHERE id = ?1",
                rusqlite::params!["sess-b"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert!(sess_b_ended, "sess-b should be ended");
        assert_eq!(bytes, 500);
        assert_eq!(frames, 10);
        assert_eq!(errors, 1);
    }
}
