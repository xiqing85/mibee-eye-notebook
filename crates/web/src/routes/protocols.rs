//! Protocol configuration endpoints.
//!
//! All handlers require authentication (enforced by middleware).
//! Configs are persisted in the SQLite `protocol_configs` table, seeded
//! from `config.toml` at first startup. Each protocol has a well-known
//! schema used for type validation on update.

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rusqlite::Connection;
use std::sync::Arc;

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;
use tokio::sync::Mutex;

/// Type alias for the shared DB connection.
type Db = Arc<Mutex<Connection>>;

// ---------------------------------------------------------------------------
// Schema definitions — used to validate PUT payloads field-by-field.
// Numeric / boolean fields sent as strings are coerced (for backward
// compatibility with older frontends). Unknown fields are rejected.
// ---------------------------------------------------------------------------

/// Field type for protocol config schema validation.
enum FieldType {
    Bool,
    U16,
    U32,
    String_,
}

impl FieldType {
    /// Coerce a JSON value into this field type, or reject.
    /// Returns Ok(coerced) if the value matches or can be coerced;
    /// returns Err(message) if the value cannot be coerced.
    fn coerce(&self, v: serde_json::Value) -> Result<serde_json::Value, String> {
        match self {
            FieldType::Bool => match v {
                serde_json::Value::Bool(_) => Ok(v),
                // Accept string "true" / "false" from legacy frontends.
                serde_json::Value::String(s) => match s.parse::<bool>() {
                    Ok(b) => Ok(serde_json::Value::Bool(b)),
                    Err(_) => Err(format!("expected bool, got string {:?}", s)),
                },
                other => Err(format!("expected bool, got {}", type_name(&other))),
            },
            FieldType::U16 => coerce_uint(v, 0, u16::MAX as u64).map(serde_json::Value::from),
            FieldType::U32 => coerce_uint(v, 0, u32::MAX as u64).map(serde_json::Value::from),
            FieldType::String_ => match v {
                serde_json::Value::String(_) => Ok(v),
                other => Err(format!("expected string, got {}", type_name(&other))),
            },
        }
    }
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Coerce a JSON number-or-numeric-string into u64, bounded by [min, max].
fn coerce_uint(v: serde_json::Value, min: u64, max: u64) -> Result<u64, String> {
    match v {
        serde_json::Value::Number(n) => {
            let as_u64 = n
                .as_u64()
                .ok_or_else(|| format!("expected integer, got float {:?}", n))?;
            if as_u64 < min || as_u64 > max {
                return Err(format!(
                    "integer {} out of range [{}, {}]",
                    as_u64, min, max
                ));
            }
            Ok(as_u64)
        }
        // Accept numeric strings from legacy frontends.
        serde_json::Value::String(s) => {
            let parsed: i64 = s
                .parse()
                .map_err(|_| format!("expected integer, got string {:?}", s))?;
            if parsed < min as i64 || parsed > max as i64 {
                return Err(format!(
                    "integer {} out of range [{}, {}]",
                    parsed, min, max
                ));
            }
            Ok(parsed as u64)
        }
        other => Err(format!("expected integer, got {}", type_name(&other))),
    }
}

/// Per-protocol schema: maps each known field to its expected type.
/// Unknown keys are rejected on update (forces explicit schema evolution).
fn schema_for(protocol: &str) -> &'static [(&'static str, FieldType)] {
    match protocol {
        "onvif" => &[
            ("enabled", FieldType::Bool),
            ("device_name", FieldType::String_),
            ("manufacturer", FieldType::String_),
            ("model", FieldType::String_),
            ("serial", FieldType::String_),
            ("firmware_version", FieldType::String_),
        ],
        "gb28181" => &[
            ("enabled", FieldType::Bool),
            ("platform_sip_address", FieldType::String_),
            ("platform_sip_port", FieldType::U16),
            ("device_id", FieldType::String_),
            ("username", FieldType::String_),
            ("password", FieldType::String_),
            ("sip_domain", FieldType::String_),
            ("register_interval_secs", FieldType::U32),
        ],
        "rtmp_push" => &[
            ("enabled", FieldType::Bool),
            ("push_url", FieldType::String_),
            ("app_name", FieldType::String_),
            ("stream_name", FieldType::String_),
            ("reconnect_interval_secs", FieldType::U32),
            ("max_reconnect_attempts", FieldType::U32),
        ],
        _ => &[],
    }
}

/// Validate + coerce a partial update payload against the schema.
/// Returns the coerced object containing only known fields.
/// Unknown fields trigger an error.
fn validate_and_coerce(
    protocol: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let obj = match payload.as_object() {
        Some(o) => o,
        None => {
            return Err(format!(
                "expected JSON object for {} config update, got {}",
                protocol,
                type_name(payload)
            ));
        }
    };

    let schema = schema_for(protocol);
    let schema_keys: std::collections::HashSet<&str> = schema.iter().map(|(k, _)| *k).collect();

    // Reject unknown keys (forces explicit schema evolution).
    for k in obj.keys() {
        if !schema_keys.contains(k.as_str()) {
            return Err(format!(
                "unknown field {:?} for {} config; if this is a new setting, add it to schema_for() first",
                k, protocol
            ));
        }
    }

    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        // Find the field's expected type.
        let field_type = schema
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, t)| t)
            .expect("schema lookup after unknown-key check");
        let coerced = field_type
            .coerce(v.clone())
            .map_err(|e| format!("field {:?}: {}", k, e))?;
        out.insert(k.clone(), coerced);
    }
    Ok(serde_json::Value::Object(out))
}

/// Shallow-merge: for each key in `update`, set/overwrite in `target`.
fn merge_into(target: &mut serde_json::Value, update: serde_json::Value) {
    if let (serde_json::Value::Object(t), serde_json::Value::Object(u)) = (target, update) {
        for (k, v) in u {
            t.insert(k, v);
        }
    }
}

// ---------------------------------------------------------------------------
// Generic handler helpers
// ---------------------------------------------------------------------------

async fn handle_get(db: &Db, protocol: &str) -> axum::response::Response {
    let conn = db.lock().await;
    match crate::db::get_protocol_config(&conn, protocol) {
        Ok(Some(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(None) => ApiError::not_found(format!("{} config not found", protocol)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, protocol, "failed to read protocol config");
            ApiError::internal("database error").into_response()
        }
    }
}

async fn handle_put(
    db: &Db,
    protocol: &str,
    payload: serde_json::Value,
) -> axum::response::Response {
    // 1. Validate + coerce types field-by-field.
    let validated = match validate_and_coerce(protocol, &payload) {
        Ok(v) => v,
        Err(msg) => {
            return ApiError::bad_request(&msg).into_response();
        }
    };

    // 2. Read-merge-write inside the DB lock.
    let conn = db.lock().await;
    let mut current = match crate::db::get_protocol_config(&conn, protocol) {
        Ok(Some(v)) => v,
        Ok(None) => serde_json::Value::Object(serde_json::Map::new()),
        Err(e) => {
            tracing::error!(error = %e, protocol, "failed to read existing protocol config");
            return ApiError::internal("database error").into_response();
        }
    };
    merge_into(&mut current, validated);

    if let Err(e) = crate::db::set_protocol_config(&conn, protocol, &current) {
        tracing::error!(error = %e, protocol, "failed to persist protocol config");
        return ApiError::internal("database error").into_response();
    }

    tracing::info!(protocol, "protocol config updated via Web UI");
    (StatusCode::OK, Json(current)).into_response()
}

// ---------------------------------------------------------------------------
// ONVIF config
// ---------------------------------------------------------------------------

/// GET /api/protocols/onvif — return ONVIF device config as JSON.
#[tracing::instrument(skip_all)]
pub async fn get_protocols_onvif(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    handle_get(&db, "onvif").await
}

/// PUT /api/protocols/onvif — update ONVIF device config (partial, validated).
#[tracing::instrument(skip_all)]
pub async fn update_protocols_onvif(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    handle_put(&db, "onvif", payload).await
}

// ---------------------------------------------------------------------------
// GB28181 config
// ---------------------------------------------------------------------------

/// GET /api/protocols/gb28181 — return GB28181 device config as JSON.
#[tracing::instrument(skip_all)]
pub async fn get_protocols_gb28181(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    handle_get(&db, "gb28181").await
}

/// PUT /api/protocols/gb28181 — update GB28181 device config (partial, validated).
#[tracing::instrument(skip_all)]
pub async fn update_protocols_gb28181(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    handle_put(&db, "gb28181", payload).await
}

// ---------------------------------------------------------------------------
// RTMP push config
// ---------------------------------------------------------------------------

/// GET /api/protocols/rtmp — return RTMP push config as JSON.
#[tracing::instrument(skip_all)]
pub async fn get_protocols_rtmp(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    handle_get(&db, "rtmp_push").await
}

/// PUT /api/protocols/rtmp — update RTMP push config (partial, validated).
#[tracing::instrument(skip_all)]
pub async fn update_protocols_rtmp(
    Extension(db): Extension<Db>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    handle_put(&db, "rtmp_push", payload).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_coerce_bool_accepts_native() {
        assert_eq!(FieldType::Bool.coerce(json!(true)).unwrap(), json!(true));
    }

    #[test]
    fn test_coerce_bool_coerces_string() {
        // Legacy frontend sends "true"/"false" strings.
        assert_eq!(FieldType::Bool.coerce(json!("true")).unwrap(), json!(true));
        assert_eq!(
            FieldType::Bool.coerce(json!("false")).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn test_coerce_bool_rejects_invalid() {
        assert!(FieldType::Bool.coerce(json!("yes")).is_err());
        assert!(FieldType::Bool.coerce(json!(1)).is_err());
        assert!(FieldType::Bool.coerce(json!("null")).is_err());
    }

    #[test]
    fn test_coerce_u16_native() {
        assert_eq!(FieldType::U16.coerce(json!(5060)).unwrap(), json!(5060));
        assert_eq!(FieldType::U16.coerce(json!(0)).unwrap(), json!(0));
        assert_eq!(FieldType::U16.coerce(json!(65535)).unwrap(), json!(65535));
    }

    #[test]
    fn test_coerce_u16_coerces_numeric_string() {
        // Legacy frontend sends port as "5060".
        assert_eq!(FieldType::U16.coerce(json!("5060")).unwrap(), json!(5060));
    }

    #[test]
    fn test_coerce_u16_rejects_out_of_range() {
        assert!(FieldType::U16.coerce(json!(65536)).is_err());
        assert!(FieldType::U16.coerce(json!(-1)).is_err());
    }

    #[test]
    fn test_validate_rejects_unknown_field() {
        let payload = json!({"enabled": true, "bogus_field": "evil"});
        let err = validate_and_coerce("onvif", &payload).unwrap_err();
        assert!(err.contains("unknown field"), "got: {}", err);
        assert!(err.contains("bogus_field"), "got: {}", err);
    }

    #[test]
    fn test_validate_coerces_legacy_string_types() {
        // Simulate a legacy frontend sending all-string values.
        let payload = json!({
            "enabled": "true",
            "platform_sip_port": "5060",
            "register_interval_secs": "60"
        });
        let result = validate_and_coerce("gb28181", &payload).unwrap();
        assert_eq!(result["enabled"], json!(true));
        assert_eq!(result["platform_sip_port"], json!(5060));
        assert_eq!(result["register_interval_secs"], json!(60));
    }

    #[test]
    fn test_validate_rejects_wrong_type_for_field() {
        let payload = json!({"enabled": "not-a-bool"});
        let err = validate_and_coerce("onvif", &payload).unwrap_err();
        assert!(err.contains("expected bool"), "got: {}", err);

        let payload = json!({"platform_sip_port": "not-a-number"});
        let err = validate_and_coerce("gb28181", &payload).unwrap_err();
        assert!(err.contains("expected integer"), "got: {}", err);
    }

    #[test]
    fn test_validate_partial_update_keeps_known_only() {
        let payload = json!({"device_name": "kitchen-cam"});
        let result = validate_and_coerce("onvif", &payload).unwrap();
        assert_eq!(result["device_name"], json!("kitchen-cam"));
        // Only the provided field is in the validated output.
        assert_eq!(result.as_object().unwrap().len(), 1);
    }

    #[test]
    fn test_schema_for_unknown_protocol_is_empty() {
        assert!(schema_for("nonexistent").is_empty());
    }
}
