use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use sqlx::{SqlitePool, query, query_as, Pool, Sqlite, sqlite::SqliteConnectOptions};
use sqlx::sqlite::SqlitePoolOptions;
use futures::stream::StreamExt;

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
    /// When the physical device went offline (NULL when online).
    /// Set by the hot-plug monitor when udev detects device removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline_since: Option<String>,
}

/// Initialize the SQLite pool at `path` with WAL mode and busy timeout.
///
/// This creates an async SqlitePool for web CRUD operations.
pub async fn init_pool(path: &str) -> Result<SqlitePool> {
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(connect_options)
        .await
        .context("Failed to create SQLite pool")?;

    // Enable WAL mode and busy timeout
    sqlx::query("PRAGMA journal_mode=WAL;")
        .execute(&pool)
        .await
        .context("Failed to set WAL mode")?;

    sqlx::query("PRAGMA busy_timeout=5000;")
        .execute(&pool)
        .await
        .context("Failed to set busy timeout")?;

    run_migrations(&pool).await.context("Failed to run database migrations")?;

    Ok(pool)
}

/// Initialize the blocking Connection for the security crate.
///
/// This is used for blocking rusqlite operations (auth, sessions).
pub fn init_auth_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).context("Failed to open SQLite database for auth")?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .context("Failed to set WAL mode")?;

    conn.execute_batch("PRAGMA busy_timeout=5000;")
        .context("Failed to set busy timeout")?;

    Ok(conn)
}

/// Ensure the schema_version table exists, read current version, apply pending
/// migrations, and update the version.
pub(crate) async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    // Create version tracking table if it doesn't exist
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
        .execute(pool)
        .await
        .context("Failed to create schema_version table")?;

    // Get current version (0 if none)
    let current_version: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM schema_version")
        .fetch_one(pool)
        .await
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

        sqlx::query(&sql)
            .execute(pool)
            .await
            .with_context(|| format!("Failed to apply migration {}", num))?;

        sqlx::query("INSERT INTO schema_version (version) VALUES (?1)")
            .bind(num)
            .execute(pool)
            .await
            .context("Failed to update schema_version")?;

        tracing::info!(migration = num, "Applied database migration");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Camera CRUD
// ---------------------------------------------------------------------------

pub async fn create_camera(pool: &SqlitePool, camera: &CameraRow) -> Result<()> {
    let config_str = serde_json::to_string(&camera.config).context("Failed to serialize config")?;

    sqlx::query(
        "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at, offline_since)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(&camera.id)
    .bind(&camera.name)
    .bind(&camera.camera_type)
    .bind(&config_str)
    .bind(&camera.status)
    .bind(&camera.created_at)
    .bind(&camera.updated_at)
    .bind(&camera.offline_since)
    .execute(pool)
    .await
    .context("Failed to create camera")?;

    Ok(())
}

pub async fn get_camera(pool: &SqlitePool, id: &str) -> Result<Option<CameraRow>> {
    let row = sqlx::query_as::<_, (String, String, String, String, String, String, String, Option<String>)>(
        "SELECT id, name, camera_type, config, status, created_at, updated_at, offline_since
         FROM cameras WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("Failed to get camera")?;

    match row {
        Some((id, name, camera_type, config_str, status, created_at, updated_at, offline_since)) => {
            let config: serde_json::Value =
                serde_json::from_str(&config_str).context("Failed to parse camera config JSON")?;
            Ok(Some(CameraRow {
                id,
                name,
                camera_type,
                config,
                status,
                created_at,
                updated_at,
                offline_since,
            }))
        }
        None => Ok(None),
    }
}

pub async fn list_cameras(pool: &SqlitePool) -> Result<Vec<CameraRow>> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, String, String, Option<String>)>(
        "SELECT id, name, camera_type, config, status, created_at, updated_at, offline_since
         FROM cameras ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("Failed to list cameras")?;

    let mut cameras = Vec::new();
    for (id, name, camera_type, config_str, status, created_at, updated_at, offline_since) in rows {
        let config: serde_json::Value = serde_json::from_str(&config_str)
            .context("Failed to parse camera config JSON")?;
        cameras.push(CameraRow {
            id,
            name,
            camera_type,
            config,
            status,
            created_at,
            updated_at,
            offline_since,
        });
    }

    Ok(cameras)
}

/// Auto-discover physically attached video devices and create camera entries
/// for any that don't already exist in the database.
///
/// This runs on startup so the user sees their webcam in the dashboard
/// immediately without manual configuration.
pub async fn auto_discover_cameras(pool: &SqlitePool) -> Result<usize> {
    let devices = match capture::video::enumerate_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "failed to enumerate video devices during auto-discovery");
            return Ok(0);
        }
    };

    let existing = list_cameras(pool).await?;
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
            tracing::debug!(
                index = dev.index,
                "skipping device with no formats (likely metadata node)"
            );
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
            offline_since: None,
        };

        match create_camera(pool, &camera).await {
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

/// Mark all USB cameras with the given device index as offline.
///
/// Called by the hot-plug monitor when udev detects device removal.
/// Sets `status = 'offline'` and `offline_since = datetime('now')`.
/// Returns the IDs of the affected cameras so the caller can stop
/// their active streams (if any) and broadcast SSE events.
pub async fn mark_cameras_offline_by_device_index(
    pool: &SqlitePool,
    device_index: i64,
) -> Result<Vec<String>> {
    // First fetch matching camera IDs so the caller can stop streams.
    let cameras = list_cameras(pool).await?;
    let affected: Vec<String> = cameras
        .into_iter()
        .filter(|c| {
            c.camera_type == "usb"
                && c.status != "offline"
                && c.config.get("device_index").and_then(|v| v.as_i64()) == Some(device_index)
        })
        .map(|c| c.id)
        .collect();

    if affected.is_empty() {
        return Ok(vec![]);
    }

    // Bulk-update via json_extract (SQLite ≥ 3.38) or config LIKE.
    // We already know the IDs, so update them individually for clarity.
    let now = chrono_epoch_secs();
    for id in &affected {
        sqlx::query(
            "UPDATE cameras SET status = 'offline', offline_since = ?1, updated_at = ?1
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to mark camera offline")?;
    }

    tracing::info!(device_index, count = affected.len(), "marked cameras offline");
    Ok(affected)
}

/// Clear the offline flag for cameras with the given device index.
///
/// Called by the hot-plug monitor when a device is plugged back in.
/// Resets `status` to `'stopped'` (does NOT auto-start) and clears
/// `offline_since`. Returns the IDs of the re-activated cameras.
pub async fn clear_offline_by_device_index(
    pool: &SqlitePool,
    device_index: i64,
) -> Result<Vec<String>> {
    let cameras = list_cameras(pool).await?;
    let affected: Vec<String> = cameras
        .into_iter()
        .filter(|c| {
            c.camera_type == "usb"
                && c.status == "offline"
                && c.config.get("device_index").and_then(|v| v.as_i64()) == Some(device_index)
        })
        .map(|c| c.id)
        .collect();

    if affected.is_empty() {
        return Ok(vec![]);
    }

    let now = chrono_epoch_secs();
    for id in &affected {
        sqlx::query(
            "UPDATE cameras SET status = 'stopped', offline_since = NULL, updated_at = ?1
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to clear offline status")?;
    }

    tracing::info!(device_index, count = affected.len(), "cleared offline status");
    Ok(affected)
}

pub async fn update_camera(pool: &SqlitePool, camera: &CameraRow) -> Result<()> {
    let config_str = serde_json::to_string(&camera.config).context("Failed to serialize config")?;

    let result = sqlx::query(
        "UPDATE cameras SET name = ?1, camera_type = ?2, config = ?3,
                status = ?4, updated_at = ?5, offline_since = ?6
         WHERE id = ?7",
    )
    .bind(&camera.name)
    .bind(&camera.camera_type)
    .bind(&config_str)
    .bind(&camera.status)
    .bind(&camera.updated_at)
    .bind(&camera.offline_since)
    .bind(&camera.id)
    .execute(pool)
    .await
    .context("Failed to update camera")?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Camera with id '{}' not found", camera.id);
    }

    Ok(())
}

pub async fn delete_camera(pool: &SqlitePool, id: &str) -> Result<()> {
    let result = sqlx::query("DELETE FROM cameras WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete camera")?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Camera with id '{}' not found", id);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Settings CRUD
// ---------------------------------------------------------------------------

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let value = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("Failed to get setting")?;

    Ok(value)
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .context("Failed to set setting")?;

    Ok(())
}

/// List all settings as a vector of (key, value) pairs.
pub async fn list_settings(pool: &SqlitePool) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM settings ORDER BY key"
    )
    .fetch_all(pool)
    .await
    .context("Failed to list settings")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Protocol configs (onvif / gb28181 / rtmp_push)
// ---------------------------------------------------------------------------

/// Retrieve one protocol's config as a JSON Value, or None if not stored.
pub async fn get_protocol_config(pool: &SqlitePool, key: &str) -> Result<Option<serde_json::Value>> {
    let value_str: Option<String> = sqlx::query_scalar("SELECT value FROM protocol_configs WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("Failed to get protocol config")?;

    match value_str {
        Some(s) => {
            let v: serde_json::Value =
                serde_json::from_str(&s).context("Failed to parse protocol config JSON")?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// Upsert one protocol config (replaces existing value, updates timestamp).
pub async fn set_protocol_config(pool: &SqlitePool, key: &str, value: &serde_json::Value) -> Result<()> {
    let s = serde_json::to_string(value).context("Failed to serialize protocol config")?;
    let sql = "INSERT INTO protocol_configs (key, value, updated_at) VALUES (?1, ?2, datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at";
    sqlx::query(sql)
        .bind(key)
        .bind(s)
        .execute(pool)
        .await
        .context("Failed to upsert protocol config")?;
    Ok(())
}

/// Return all stored protocol configs as (key, value) pairs, ordered by key.
pub async fn list_protocol_configs(pool: &SqlitePool) -> Result<Vec<(String, serde_json::Value)>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM protocol_configs ORDER BY key"
    )
    .fetch_all(pool)
    .await
    .context("Failed to list protocol configs")?;

    let mut out = Vec::new();
    for (key, value_str) in rows {
        let value: serde_json::Value = serde_json::from_str(&value_str)
            .context("Failed to parse protocol config JSON")?;
        out.push((key, value));
    }
    Ok(out)
}

/// Return true if the protocol_configs table has zero rows (used at startup
/// to decide whether to seed from config.toml).
pub async fn protocol_configs_is_empty(pool: &SqlitePool) -> Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM protocol_configs")
        .fetch_one(pool)
        .await
        .context("Failed to check if protocol_configs is empty")?;
    Ok(count == 0)
}

// ---------------------------------------------------------------------------
// Stream Session logging
// ---------------------------------------------------------------------------

/// Insert a new stream session row for audit purposes.
/// The session starts now with default zero counters.
pub async fn insert_stream_session(pool: &SqlitePool, session_id: &str, camera_id: &str) -> Result<()> {
    sqlx::query("INSERT INTO stream_sessions (id, camera_id) VALUES (?1, ?2)")
        .bind(session_id)
        .bind(camera_id)
        .execute(pool)
        .await
        .context("Failed to insert stream session")?;
    Ok(())
}

/// Finalize (end) the most recent open session for a camera.
/// Updates stats if provided, otherwise records 0s.
/// Returns the number of rows updated (0 or 1).
pub async fn finalize_stream_session(
    pool: &SqlitePool,
    camera_id: &str,
    bytes_received: i64,
    frames_received: i64,
    error_count: i64,
) -> Result<usize> {
    let result = sqlx::query(
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
    )
    .bind(bytes_received)
    .bind(frames_received)
    .bind(error_count)
    .bind(camera_id)
    .execute(pool)
    .await
    .context("Failed to finalize stream session")?;

    Ok(result.rows_affected() as usize)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Executor;

    /// Create an in-memory database pool for testing.
    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();

        // Run migrations manually (001 for schema, 004 for offline_since column)
        sqlx::query(include_str!("../../../migrations/001_initial.sql"))
            .await
            .unwrap();
        sqlx::query(include_str!("../../../migrations/004__add_offline_since.sql"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO schema_version (version) VALUES (4)")
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn test_migration_tables_exist() {
        let pool = test_pool().await;

        // Verify all tables were created
        let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();

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

    #[tokio::test]
    async fn test_camera_crud_roundtrip() {
        let pool = test_pool().await;

        let camera = CameraRow {
            id: "cam-001".to_string(),
            name: "Front Door".to_string(),
            camera_type: "rtsp".to_string(),
            config: serde_json::json!({"url": "rtsp://192.168.1.100:554/stream1"}),
            status: "stopped".to_string(),
            created_at: "2025-01-01T00:00:00".to_string(),
            updated_at: "2025-01-01T00:00:00".to_string(),
            offline_since: None,
        };

        // Create
        create_camera(&pool, &camera).await.unwrap();

        // Read
        let fetched = get_camera(&pool, "cam-001")
            .await
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
        update_camera(&pool, &updated).await.unwrap();

        let fetched = get_camera(&pool, "cam-001")
            .await
            .unwrap()
            .expect("Camera should exist after update");
        assert_eq!(fetched.name, "Back Door");
        assert_eq!(fetched.status, "running");

        // List
        let all = list_cameras(&pool).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "cam-001");

        // Delete
        delete_camera(&pool, "cam-001").await.unwrap();
        let fetched = get_camera(&pool, "cam-001").await.unwrap();
        assert!(fetched.is_none(), "Camera should be deleted");

        // List empty
        let all = list_cameras(&pool).await.unwrap();
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn test_settings_get_set() {
        let pool = test_pool().await;

        // Get non-existent key
        let val = get_setting(&pool, "nonexistent").await.unwrap();
        assert!(val.is_none());

        // Set and get
        set_setting(&pool, "theme", "dark").await.unwrap();
        let val = get_setting(&pool, "theme").await.unwrap();
        assert_eq!(val, Some("dark".to_string()));

        // Update
        set_setting(&pool, "theme", "light").await.unwrap();
        let val = get_setting(&pool, "theme").await.unwrap();
        assert_eq!(val, Some("light".to_string()));

        // Multiple settings
        set_setting(&pool, "language", "zh-CN").await.unwrap();
        let val = get_setting(&pool, "language").await.unwrap();
        assert_eq!(val, Some("zh-CN".to_string()));

        // Original still intact
        let val = get_setting(&pool, "theme").await.unwrap();
        assert_eq!(val, Some("light".to_string()));
    }

    #[tokio::test]
    async fn test_delete_nonexistent_camera_fails() {
        let pool = test_pool().await;
        let result = delete_camera(&pool, "no-such-camera").await;
        assert!(result.is_err(), "Deleting nonexistent camera should fail");
    }

    #[tokio::test]
    async fn test_update_nonexistent_camera_fails() {
        let pool = test_pool().await;
        let camera = CameraRow {
            id: "no-such".to_string(),
            name: "Ghost".to_string(),
            camera_type: "rtsp".to_string(),
            config: serde_json::Value::Object(Default::default()),
            status: "stopped".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
            offline_since: None,
        };
        let result = update_camera(&pool, &camera).await;
        assert!(result.is_err(), "Updating nonexistent camera should fail");
    }

    #[tokio::test]
    async fn test_cameras_have_type_index() {
        let pool = test_pool().await;
        let indexes: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='cameras'")
            .fetch_all(&pool)
            .await
            .unwrap();

        assert!(
            indexes.contains(&"idx_cameras_type".to_string()),
            "idx_cameras_type index should exist"
        );
    }

    // -----------------------------------------------------------------------
    // Stream Session tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_insert_and_finalize_stream_session() {
        let pool = test_pool().await;

        // Create a camera first (FK constraint)
        sqlx::query(
            concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            ),
        )
        .bind("cam-001")
        .bind("Front Door")
        .bind("usb")
        .bind("{}")
        .bind("stopped")
        .bind("now")
        .bind("now")
        .execute(&pool)
        .await
        .unwrap();

        // Insert a stream session
        insert_stream_session(&pool, "sess-001", "cam-001").await.unwrap();

        // Verify the row exists with ended_at IS NULL
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stream_sessions
             WHERE camera_id = ?1 AND ended_at IS NULL",
        )
        .bind("cam-001")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "should have one open session");

        // Finalize the session with stats
        let updated = finalize_stream_session(&pool, "cam-001", 1024, 30, 0).await.unwrap();
        assert_eq!(updated, 1, "should update exactly one row");

        // Verify the row now has ended_at NOT NULL and stats recorded
        let (ended_count, bytes, frames, errors): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), bytes_received, frames_received, error_count
             FROM stream_sessions
             WHERE camera_id = ?1 AND ended_at IS NOT NULL",
        )
        .bind("cam-001")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ended_count, 1, "should have one ended session");
        assert_eq!(bytes, 1024);
        assert_eq!(frames, 30);
        assert_eq!(errors, 0);
    }

    #[tokio::test]
    async fn test_insert_stream_session_uses_defaults() {
        let pool = test_pool().await;

        // Create a camera first (FK constraint)
        sqlx::query(
            concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES ('cam-002', 'Test', 'usb', '{}', 'stopped', datetime('now'), datetime('now'));",
            ),
        )
        .execute(&pool)
        .await
        .unwrap();

        insert_stream_session(&pool, "sess-002", "cam-002").await.unwrap();

        // Verify default values for numeric columns
        let (bytes, frames, errors): (i64, i64, i64) = sqlx::query_as(
            "SELECT bytes_received, frames_received, error_count
             FROM stream_sessions WHERE id = ?1",
        )
        .bind("sess-002")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(bytes, 0, "bytes_received should default to 0");
        assert_eq!(frames, 0, "frames_received should default to 0");
        assert_eq!(errors, 0, "error_count should default to 0");
    }

    #[tokio::test]
    async fn test_finalize_only_latest_open_session() {
        let pool = test_pool().await;

        // Create a camera first (FK constraint)
        sqlx::query(
            concat!(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at) ",
            "VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now'))",
            ),
        )
        .bind("cam-001")
        .bind("Front Door")
        .bind("usb")
        .bind("{}")
        .bind("stopped")
        .execute(&pool)
        .await
        .unwrap();

        // Insert two open sessions with explicit timestamps to ensure ordering
        sqlx::query(
            concat!(
                "INSERT INTO stream_sessions (id, camera_id, started_at) ",
                "VALUES (?1, ?2, ?3)",
            ),
        )
        .bind("sess-a")
        .bind("cam-001")
        .bind("2025-01-01T00:00:00")
        .execute(&pool)
        .await
        .unwrap();
        
        sqlx::query(
            concat!(
                "INSERT INTO stream_sessions (id, camera_id, started_at) ",
                "VALUES (?1, ?2, ?3)",
            ),
        )
        .bind("sess-b")
        .bind("cam-001")
        .bind("2025-01-02T00:00:00")
        .execute(&pool)
        .await
        .unwrap();

        // Finalize — should only affect the latest (sess-b)
        let updated = finalize_stream_session(&pool, "cam-001", 500, 10, 1).await.unwrap();
        assert_eq!(updated, 1, "should update exactly one row");

        // sess-a should still be open
        let open_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stream_sessions
             WHERE camera_id = ?1 AND ended_at IS NULL",
        )
        .bind("cam-001")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(open_count, 1, "sess-a should still be open");

        // The finalized one should have the stats
        let (sess_b_ended, bytes, frames, errors): (bool, i64, i64, i64) = sqlx::query_as(
            "SELECT ended_at IS NOT NULL, bytes_received, frames_received, error_count
             FROM stream_sessions WHERE id = ?1",
        )
        .bind("sess-b")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(sess_b_ended, "sess-b should be ended");
        assert_eq!(bytes, 500);
        assert_eq!(frames, 10);
        assert_eq!(errors, 1);
    }
}

/// Test helper: creates in-memory SqlitePool and auth_db for testing
/// Returns (pool, auth_db) where pool is for web CRUD and auth_db is for security operations
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex;

#[cfg(test)]
pub fn create_test_dbs() -> (SqlitePool, Arc<Mutex<rusqlite::Connection>>) {
    let pool = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(SqlitePool::connect_with(
            SqliteConnectOptions::from("sqlite::memory:")
                .create_if_missing(true)
        ))
    }).expect("Failed to create test pool");
    
    // Create in-memory connection for auth operations
    let auth_db = Arc::new(Mutex::new(
        rusqlite::Connection::open_in_memory().expect("Failed to create test auth db")
    ));
    
    (pool, auth_db)
}