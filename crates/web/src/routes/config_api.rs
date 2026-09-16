//! Unified config + status endpoints (SPEC v1 §3, §5).
//!
//! `GET/PUT /api/config` folds the former `/api/settings` dotted-key bag and
//! the `/api/protocols/*` section endpoints into one document:
//!
//! ```json
//! {
//!   "settings":  { "ui": {"theme": "dark"}, "web": {"port": "8443"}, … },
//!   "protocols": { "onvif": {…}, "gb28181": {…}, "rtmp": {…},
//!                  "recording": {…}, "webrtc": {…} }
//! }
//! ```
//!
//! PUT accepts a partial document (deep merge). Protocol sections go through
//! the same validate/coerce/persist path as before and hot-toggle the
//! corresponding runtime (ONVIF/GB28181 start/stop) — semantics unchanged,
//! `config_apply.default = "immediate"`.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::db::{self};
use crate::errors::ApiError;
use crate::protocol_runtime::{
    ProtocolRuntime, build_onvif_config_from_json, extract_gb28181_config,
};
use crate::routes::protocols::Db;
use crate::stream_manager::StreamManager;

use super::protocols::validate_and_coerce;

static PROCESS_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

// ---------------------------------------------------------------------------
// GET /api/status (SPEC §3)
// ---------------------------------------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn status_handler(
    Extension(db): Extension<Db>,
    Extension(advertised_host): Extension<Arc<String>>,
) -> Response {
    let start = PROCESS_START.get_or_init(std::time::Instant::now);
    let cameras = db::list_cameras(&db)
        .await
        .map(|rows| rows.len())
        .unwrap_or(0);
    (
        StatusCode::OK,
        Json(json!({
            "device_name": "mibee-eye",
            "model": format!("notebook ({advertised_host})"),
            "vendor": "MiBee Studio",
            "firmware": env!("CARGO_PKG_VERSION"),
            "uptime": start.elapsed().as_secs(),
            "cameras": cameras,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/config
// ---------------------------------------------------------------------------

fn nest_dotted(rows: Vec<(String, String)>) -> Value {
    let mut root = serde_json::Map::new();
    for (key, value) in rows {
        let parts: Vec<&str> = key.split('.').collect();
        let mut node = &mut root;
        for part in &parts[..parts.len().saturating_sub(1)] {
            let entry = node
                .entry(part.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(serde_json::Map::new());
            }
            node = entry.as_object_mut().expect("just made an object");
        }
        if let Some(last) = parts.last() {
            node.insert((*last).to_string(), Value::String(value));
        }
    }
    Value::Object(root)
}

fn flatten_to_dotted(prefix: &str, value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_to_dotted(&key, v, out);
            }
        }
        Value::String(s) => out.push((prefix.to_string(), s.clone())),
        Value::Bool(b) => out.push((prefix.to_string(), b.to_string())),
        Value::Number(n) => out.push((prefix.to_string(), n.to_string())),
        other => out.push((prefix.to_string(), other.to_string())),
    }
}

#[tracing::instrument(skip_all)]
pub async fn get_config(Extension(db): Extension<Db>) -> Response {
    let settings = match db::list_settings(&db).await {
        Ok(rows) => nest_dotted(rows),
        Err(e) => {
            tracing::error!(error = %e, "failed to list settings");
            return ApiError::internal("failed to list settings").into_response();
        }
    };
    let mut protocols = serde_json::Map::new();
    for key in [
        "onvif",
        "gb28181",
        "rtmp_push",
        "recording",
        "webrtc",
        "watermark",
    ] {
        match db::get_protocol_config(&db, key).await {
            Ok(Some(v)) => {
                protocols.insert(key.to_string(), v);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, protocol = key, "failed to read protocol config");
                return ApiError::internal("database error").into_response();
            }
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "settings": settings,
            "protocols": protocols,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// PUT /api/config
// ---------------------------------------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn put_config(
    Extension(db): Extension<Db>,
    Extension(protocol_runtime): Extension<Arc<Mutex<ProtocolRuntime>>>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(advertised_host): Extension<Arc<String>>,
    Json(body): Json<Value>,
) -> Response {
    if !body.is_object() || (body.get("settings").is_none() && body.get("protocols").is_none()) {
        return ApiError::bad_request("body must contain settings and/or protocols")
            .into_response();
    }

    // -- settings section: flatten + upsert --
    if let Some(settings) = body.get("settings") {
        if !settings.is_object() {
            return ApiError::bad_request("settings must be an object").into_response();
        }
        let mut pairs = Vec::new();
        flatten_to_dotted("", settings, &mut pairs);
        for (key, value) in pairs {
            if let Err(e) = db::set_setting(&db, &key, &value).await {
                tracing::error!(error = %e, setting_key = %key, "failed to set setting");
                return ApiError::internal("failed to update settings").into_response();
            }
        }
    }

    // -- protocol sections: validate + merge + persist + hot-toggle --
    if let Some(proto_map) = body.get("protocols").and_then(|v| v.as_object()) {
        // Key alias: the canonical spec name "rtmp" maps to the DB key "rtmp_push".
        let mut rt = protocol_runtime.lock().await;
        for (name, section) in proto_map {
            let db_key = match name.as_str() {
                "rtmp" => "rtmp_push",
                other => other,
            };
            let section = section.clone();
            let validated = match validate_and_coerce(db_key, &section) {
                Ok(v) => v,
                Err(msg) => return ApiError::bad_request(&msg).into_response(),
            };
            let mut current = match db::get_protocol_config(&db, db_key).await {
                Ok(Some(v)) => v,
                Ok(None) => json!({}),
                Err(e) => {
                    tracing::error!(error = %e, protocol = db_key, "failed to read protocol config");
                    return ApiError::internal("database error").into_response();
                }
            };
            if let Some(obj) = validated.as_object() {
                for (k, v) in obj {
                    current[k.as_str()] = v.clone();
                }
            }
            // SPEC §5.2 semantic checks over the MERGED blob (types, ranges
            // and enums are already schema-validated above) — before persist,
            // so a rejected update leaves the stored config untouched:
            // enabled requires content; timestamp format whitelist.
            if db_key == "watermark"
                && let Err(msg) = crate::config::validate_watermark_blob(&current)
            {
                return ApiError::bad_request(&msg).into_response();
            }
            if let Err(e) = db::set_protocol_config(&db, db_key, &current).await {
                tracing::error!(error = %e, protocol = db_key, "failed to persist protocol config");
                return ApiError::internal("database error").into_response();
            }
            tracing::info!(protocol = db_key, "protocol config updated via /api/config");

            // Hot-toggle exactly like the former per-protocol endpoints.
            if db_key == "onvif" {
                let enabled = current
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if enabled {
                    let cfg = build_onvif_config_from_json(&current, &advertised_host);
                    if let Err(e) = rt.start_onvif(cfg, stream_manager.clone()).await {
                        tracing::warn!(error = %e, "failed to start ONVIF after config update");
                    }
                } else {
                    rt.stop_onvif().await;
                }
            } else if db_key == "gb28181" {
                let enabled = current
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if enabled {
                    let cfg = extract_gb28181_config(&current);
                    if let Err(e) = rt.start_gb28181(&cfg, stream_manager.clone()).await {
                        tracing::warn!(error = %e, "failed to start GB28181 after config update");
                    }
                } else {
                    rt.stop_gb28181().await;
                }
            }
            // rtmp_push / recording / webrtc are read at use time — no action.
        }
    }

    (StatusCode::OK, Json(json!({"applied": "immediate"}))).into_response()
}

// ---------------------------------------------------------------------------
// Router helper
// ---------------------------------------------------------------------------

pub fn routes() -> Router {
    Router::new()
        .route("/api/status", get(status_handler))
        .route("/api/config", get(get_config).put(put_config))
}
