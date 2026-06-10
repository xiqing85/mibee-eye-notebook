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
}
