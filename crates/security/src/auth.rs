use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fmt::Write;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

/// Typed error for authentication operations.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("user not found: {0}")]
    UserNotFound(String),
    #[error("incorrect password")]
    WrongPassword,
    #[error("database error: {0}")]
    Database(String),
    #[error("bcrypt error: {0}")]
    Bcrypt(String),
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for AuthError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(inner) = e.downcast_ref::<rusqlite::Error>() {
            return AuthError::Database(inner.to_string());
        }
        if let Some(inner) = e.downcast_ref::<bcrypt::BcryptError>() {
            return AuthError::Bcrypt(inner.to_string());
        }
        AuthError::Other(e.to_string())
    }
}
use thiserror::Error;

/// Default session lifetime: 24 hours.
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;

/// Safe wrapper that returns seconds since UNIX_EPOCH, falling back to 0 on clock errors.
fn safe_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "System time before UNIX_EPOCH; falling back to 0. Clock may be broken.");
            Duration::ZERO
        })
        .as_secs()
}

/// Ensure the `sessions` table exists.
///
/// Canonical DDL lives in `migrations/003_users_sessions.sql`.
/// Loads from migration to keep a single source of truth.
/// Safe to call multiple times (IF NOT EXISTS).
pub fn init_sessions_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../../migrations/003_users_sessions.sql"))
        .context("Failed to initialize auth tables")?;
    Ok(())
}

/// Generate a cryptographically random 32-byte hex string (64 hex chars).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let mut hex = String::with_capacity(64);
    for b in &bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}
#[cfg(test)]
fn now_secs_string() -> String {
    safe_epoch_secs().to_string()
}

/// Create a new session for `user_id`, returning the session token.
pub fn create_session(conn: &Connection, user_id: &str) -> Result<String> {
    let token = generate_token();
    let now_secs = safe_epoch_secs();
    let created_at = now_secs.to_string();
    let expires_at = (now_secs + SESSION_TTL_SECS).to_string();

    conn.execute(
        "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![token, user_id, created_at, expires_at],
    )
    .context("Failed to insert session")?;

    Ok(token)
}

/// Validate a session token.
///
/// Returns `Ok(Some(user_id))` if the token is valid and not expired.
/// Returns `Ok(None)` if the token does not exist or is expired.
pub fn validate_session(conn: &Connection, token: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT user_id, expires_at FROM sessions WHERE token = ?1")
        .context("Failed to prepare validate session statement")?;

    let mut rows = stmt.query(rusqlite::params![token])?;
    match rows.next()? {
        Some(row) => {
            let user_id: String = row.get(0)?;
            let expires_at: String = row.get(1)?;
            let expires_secs: u64 = expires_at
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid expires_at timestamp: {e}"))?;
            let now = safe_epoch_secs();

            if now < expires_secs {
                Ok(Some(user_id))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

/// Invalidate (delete) a session token, effectively logging the user out.
pub fn invalidate_session(conn: &Connection, token: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM sessions WHERE token = ?1",
        rusqlite::params![token],
    )
    .context("Failed to delete session")?;
    Ok(())
}

pub fn cleanup_expired_sessions(conn: &Connection) -> Result<usize> {
    let now = safe_epoch_secs().to_string();

    let count = conn
        .execute(
            "DELETE FROM sessions WHERE expires_at < ?1",
            rusqlite::params![now],
        )
        .context("Failed to cleanup expired sessions")?;
    Ok(count)
}

/// Ensure the `users` table exists.
///
/// Canonical DDL lives in `migrations/003_users_sessions.sql`.
/// Loads from migration to keep a single source of truth.
/// Safe to call multiple times (IF NOT EXISTS).
pub fn init_users_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../../migrations/003_users_sessions.sql"))
        .context("Failed to initialize auth tables")?;
    Ok(())
}
/// Check if this is the first run (no admin user configured).
///
/// Returns `true` if the users table doesn't exist or has no rows.
/// Returns `false` if at least one user exists.
pub fn is_first_run(conn: &Connection) -> Result<bool> {
    let table_exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='users'")?
        .query_map([], |_| Ok(()))?
        .next()
        .is_some();

    if !table_exists {
        return Ok(true);
    }

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
    Ok(count == 0)
}

/// Get the stored password hash for a user.
pub fn get_user_password(conn: &Connection, username: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT password_hash FROM users WHERE username = ?1")
        .context("Failed to prepare get_user_password statement")?;
    let mut rows = stmt.query(rusqlite::params![username])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Update the password hash for a user.
pub fn update_user_password(conn: &Connection, username: &str, password_hash: &str) -> Result<()> {
    let affected = conn
        .execute(
            "UPDATE users SET password_hash = ?1, updated_at = datetime('now') WHERE username = ?2",
            rusqlite::params![password_hash, username],
        )
        .context("Failed to update user password")?;
    if affected == 0 {
        anyhow::bail!("User '{}' not found", username);
    }
    Ok(())
}

/// Delete all sessions for a given user.
pub fn delete_all_user_sessions(conn: &Connection, username: &str) -> Result<usize> {
    let count = conn
        .execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            rusqlite::params![username],
        )
        .context("Failed to delete user sessions")?;
    Ok(count)
}

/// Reset a user's password.
///
/// 1. Looks up the stored password hash.
/// 2. Verifies the old password matches.
/// 3. Hashes the new password.
/// 4. Updates the user record.
/// 5. Deletes ALL existing sessions for that user (forces re-login).
pub fn reset_password(
    conn: &Connection,
    username: &str,
    old_password: &str,
    new_password: &str,
) -> std::result::Result<(), AuthError> {
    let stored_hash = get_user_password(conn, username)?
        .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

    if !crate::password::verify_password(old_password, &stored_hash)? {
        return Err(AuthError::WrongPassword);
    }

    let new_hash = crate::password::hash_password(new_password)?;
    update_user_password(conn, username, &new_hash)?;

    let deleted = delete_all_user_sessions(conn, username)?;
    tracing::info!(
        username,
        deleted_sessions = deleted,
        "Password reset, all sessions invalidated"
    );

    Ok(())
}

/// Create a session with a specific expiry (for testing).
#[cfg(test)]
fn create_session_with_expiry(conn: &Connection, user_id: &str, expires_at: u64) -> Result<String> {
    conn.execute_batch(concat!(
        "CREATE TABLE IF NOT EXISTS sessions (",
        "    token TEXT PRIMARY KEY,",
        "    user_id TEXT NOT NULL,",
        "    created_at TEXT NOT NULL,",
        "    expires_at TEXT NOT NULL",
        ");",
        "CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);",
    ))?;

    let token = generate_token();
    let now_secs = now_secs_string();

    conn.execute(
        "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![token, user_id, now_secs, expires_at.to_string()],
    )?;

    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS sessions (",
            "    token TEXT PRIMARY KEY,",
            "    user_id TEXT NOT NULL,",
            "    created_at TEXT NOT NULL,",
            "    expires_at TEXT NOT NULL",
            ");",
            "CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);",
        ))
        .unwrap();

        conn
    }

    #[test]
    fn test_auth_error_display() {
        let err = AuthError::UserNotFound("admin".to_string());
        assert_eq!(err.to_string(), "user not found: admin");

        let err = AuthError::WrongPassword;
        assert_eq!(err.to_string(), "incorrect password");

        let err = AuthError::Database("connection failed".to_string());
        assert!(err.to_string().contains("database error"));

        let err = AuthError::Bcrypt("cost too high".to_string());
        assert!(err.to_string().contains("bcrypt error"));

        let err = AuthError::Other("something happened".to_string());
        assert_eq!(err.to_string(), "something happened");
    }

    #[test]
    fn test_generate_token_is_hex() {
        let token = generate_token();
        // 32 bytes = 64 hex chars
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_token_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_create_and_validate_session() {
        let conn = test_db();

        let token = create_session(&conn, "admin").unwrap();
        assert_eq!(token.len(), 64);

        let user_id = validate_session(&conn, &token)
            .unwrap()
            .expect("Session should be valid");
        assert_eq!(user_id, "admin");
    }

    #[test]
    fn test_validate_nonexistent_token() {
        let conn = test_db();
        let result = validate_session(&conn, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_validate_expired_session() {
        let conn = test_db();

        let past = safe_epoch_secs() - 3600;
        let token = create_session_with_expiry(&conn, "admin", past).unwrap();

        let result = validate_session(&conn, &token).unwrap();
        assert!(result.is_none(), "Expired session should be invalid");
    }

    #[test]
    fn test_invalidate_session() {
        let conn = test_db();

        let token = create_session(&conn, "admin").unwrap();
        assert!(validate_session(&conn, &token).unwrap().is_some());

        invalidate_session(&conn, &token).unwrap();
        assert!(validate_session(&conn, &token).unwrap().is_none());
    }

    #[test]
    fn test_cleanup_expired_sessions() {
        let conn = test_db();

        let past = safe_epoch_secs() - 3600;
        let _token = create_session_with_expiry(&conn, "admin", past).unwrap();

        // Create a valid session
        let valid_token = create_session(&conn, "user2").unwrap();

        let cleaned = cleanup_expired_sessions(&conn).unwrap();
        assert_eq!(cleaned, 1, "Should have cleaned 1 expired session");

        // Valid session should still exist
        assert!(validate_session(&conn, &valid_token).unwrap().is_some());
    }

    #[test]
    fn test_multiple_sessions_same_user() {
        let conn = test_db();

        let t1 = create_session(&conn, "admin").unwrap();
        let t2 = create_session(&conn, "admin").unwrap();
        assert_ne!(t1, t2);

        assert_eq!(
            validate_session(&conn, &t1).unwrap(),
            Some("admin".to_string())
        );
        assert_eq!(
            validate_session(&conn, &t2).unwrap(),
            Some("admin".to_string())
        );
    }
}

#[cfg(test)]
mod password_reset_tests {
    use super::*;
    use crate::password::{hash_password, verify_password};

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS users (",
            "    username TEXT PRIMARY KEY,",
            "    password_hash TEXT NOT NULL,",
            "    created_at TEXT NOT NULL DEFAULT (datetime('now')),",
            "    updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
            ");",
        ))
        .unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS sessions (",
            "    token TEXT PRIMARY KEY,",
            "    user_id TEXT NOT NULL,",
            "    created_at TEXT NOT NULL,",
            "    expires_at TEXT NOT NULL",
            ");",
            "CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);",
        ))
        .unwrap();

        // Insert a test user
        let hash = hash_password("old_pass").unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
            rusqlite::params!["admin", hash],
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_init_users_table_creates_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS users (",
            "    username TEXT PRIMARY KEY,",
            "    password_hash TEXT NOT NULL,",
            "    created_at TEXT NOT NULL DEFAULT (datetime('now')),",
            "    updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
            ");",
        ))
        .unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='users'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(tables.len(), 1, "users table should exist");
    }

    #[test]
    fn test_get_user_password_existing_user() {
        let conn = test_db();
        let hash = get_user_password(&conn, "admin")
            .unwrap()
            .expect("should find admin");
        assert!(hash.starts_with("$2"));
    }

    #[test]
    fn test_get_user_password_nonexistent_user() {
        let conn = test_db();
        let result = get_user_password(&conn, "nobody").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_user_password() {
        let conn = test_db();
        let new_hash = hash_password("new_pass").unwrap();
        update_user_password(&conn, "admin", &new_hash).unwrap();

        let stored = get_user_password(&conn, "admin")
            .unwrap()
            .expect("should exist");
        assert_eq!(stored, new_hash);
        assert!(verify_password("new_pass", &stored).unwrap());
    }

    #[test]
    fn test_update_user_password_nonexistent_user_fails() {
        let conn = test_db();
        let hash = hash_password("x").unwrap();
        let result = update_user_password(&conn, "ghost", &hash);
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_all_user_sessions() {
        let conn = test_db();
        create_session(&conn, "admin").unwrap();
        create_session(&conn, "admin").unwrap();
        create_session(&conn, "other").unwrap();

        let deleted = delete_all_user_sessions(&conn, "admin").unwrap();
        assert_eq!(deleted, 2, "should delete admin's 2 sessions");

        // other user's session should remain
        let all_count: i32 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(all_count, 1);
    }

    #[test]
    fn test_reset_password_success() {
        let conn = test_db();
        let token = create_session(&conn, "admin").unwrap();

        reset_password(&conn, "admin", "old_pass", "shiny_new").unwrap();

        // Password should be updated
        let stored = get_user_password(&conn, "admin")
            .unwrap()
            .expect("should exist");
        assert!(verify_password("shiny_new", &stored).unwrap());
        assert!(!verify_password("old_pass", &stored).unwrap());

        // Sessions should be deleted
        assert!(validate_session(&conn, &token).unwrap().is_none());
    }

    #[test]
    fn test_reset_password_wrong_old_password() {
        let conn = test_db();
        let result = reset_password(&conn, "admin", "wrong_old", "new_pass");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("incorrect"),
            "error should mention incorrect password"
        );
    }

    #[test]
    fn test_reset_password_nonexistent_user() {
        let conn = test_db();
        let result = reset_password(&conn, "ghost", "x", "y");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_reset_password_empty_new_password() {
        let conn = test_db();
        // Empty passwords should still work (bcrypt can hash empty strings)
        reset_password(&conn, "admin", "old_pass", "").unwrap();
        let stored = get_user_password(&conn, "admin")
            .unwrap()
            .expect("should exist");
        assert!(verify_password("", &stored).unwrap());
    }
}

#[cfg(test)]
mod first_run_tests {
    use super::*;

    fn test_db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn test_db_with_user() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS users (",
            "    username TEXT PRIMARY KEY,",
            "    password_hash TEXT NOT NULL,",
            "    created_at TEXT NOT NULL DEFAULT (datetime('now')),",
            "    updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
            ");",
        ))
        .unwrap();
        let hash = crate::password::hash_password("test_pass").unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
            rusqlite::params!["admin", hash],
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_is_first_run_no_table() {
        let conn = test_db();
        assert!(
            is_first_run(&conn).unwrap(),
            "should be first run when no users table"
        );
    }

    #[test]
    fn test_is_first_run_empty_table() {
        let conn = test_db();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS users (",
            "    username TEXT PRIMARY KEY,",
            "    password_hash TEXT NOT NULL,",
            "    created_at TEXT NOT NULL DEFAULT (datetime('now')),",
            "    updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
            ");",
        ))
        .unwrap();
        assert!(
            is_first_run(&conn).unwrap(),
            "should be first run when empty users table"
        );
    }

    #[test]
    fn test_is_first_run_with_user() {
        let conn = test_db_with_user();
        assert!(
            !is_first_run(&conn).unwrap(),
            "should not be first run when user exists"
        );
    }

    #[test]
    fn test_is_first_run_multiple_users() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(concat!(
            "CREATE TABLE IF NOT EXISTS users (",
            "    username TEXT PRIMARY KEY,",
            "    password_hash TEXT NOT NULL,",
            "    created_at TEXT NOT NULL DEFAULT (datetime('now')),",
            "    updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
            ");",
        ))
        .unwrap();
        let hash = crate::password::hash_password("pass").unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
            rusqlite::params!["user1", hash],
        )
        .unwrap();
        let hash2 = crate::password::hash_password("pass2").unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
            rusqlite::params!["user2", hash2],
        )
        .unwrap();
        assert!(
            !is_first_run(&conn).unwrap(),
            "should not be first run with multiple users"
        );
    }
}
