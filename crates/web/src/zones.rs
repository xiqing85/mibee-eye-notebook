//! Zone persistence + HTTP routes (SPEC appendix A notebook dialect:
//! `GET/PUT /api/cameras/{id}/zones`).
//!
//! Zones are user-drawn polygons (intrusion/loitering) and tripwires (line
//! crossing) in video pixel coordinates. They live in the db as one
//! settings row (`zones` → `{camera_id: [Zone…]}`) and are mirrored into a
//! shared in-memory map that the main.rs zone-event engine reads on every
//! track update — a PUT applies to new events immediately.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use parking_lot::RwLock;
use serde_json::json;
use streaming::ai::zones::Zone;

use crate::db;
use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

/// In-memory mirror consumed by the zone-event engine.
pub type SharedZones = Arc<RwLock<HashMap<String, Vec<Zone>>>>;

/// Create the shared map (empty; call [`load_from_db`] at startup).
#[must_use]
pub fn new_shared() -> SharedZones {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Settings key holding the zones JSON.
const ZONES_KEY: &str = "zones";

/// Load zones from the db into the shared map (startup).
pub async fn load_from_db(pool: &sqlx::SqlitePool, shared: &SharedZones) {
    if let Ok(Some(raw)) = db::get_setting(pool, ZONES_KEY).await
        && let Ok(map) = serde_json::from_str::<HashMap<String, Vec<Zone>>>(&raw)
    {
        *shared.write() = map;
        let count: usize = shared.read().values().map(Vec::len).sum();
        tracing::info!(zones = count, "zones: loaded from db");
    }
}

async fn persist(pool: &sqlx::SqlitePool, map: &HashMap<String, Vec<Zone>>) -> anyhow::Result<()> {
    let raw = serde_json::to_string(map)?;
    db::set_setting(pool, ZONES_KEY, &raw).await?;
    Ok(())
}

/// `GET /api/cameras/{id}/zones` — list the camera's zones.
#[tracing::instrument(skip_all)]
pub async fn get_zones(
    Path(camera_id): Path<String>,
    Extension(zones): Extension<SharedZones>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let list = zones.read().get(&camera_id).cloned().unwrap_or_default();
    Ok((StatusCode::OK, axum::Json(json!({ "zones": list }))))
}

/// `PUT /api/cameras/{id}/zones` — replace the camera's zone set.
///
/// Validation: unique non-empty names, polygons have ≥3 points, tripwires
/// exactly 2. Applies immediately (the engine reads the shared map).
#[tracing::instrument(skip_all)]
pub async fn put_zones(
    Path(camera_id): Path<String>,
    Extension(zones): Extension<SharedZones>,
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
    body: axum::extract::Json<Vec<Zone>>,
) -> Result<impl IntoResponse, ApiError> {
    let list = body.0;
    let mut names = std::collections::HashSet::new();
    for z in &list {
        if z.name.trim().is_empty() {
            return Err(ApiError::bad_request("zone name must not be empty"));
        }
        if !names.insert(z.name.trim().to_string()) {
            return Err(ApiError::bad_request("duplicate zone name"));
        }
        match z.kind {
            streaming::ai::zones::ZoneKind::Intrusion => {
                if z.points.len() < 3 {
                    return Err(ApiError::bad_request("intrusion zone needs >= 3 points"));
                }
            }
            streaming::ai::zones::ZoneKind::LineCross => {
                if z.points.len() != 2 {
                    return Err(ApiError::bad_request("line zone needs exactly 2 points"));
                }
            }
        }
    }
    let mut map = zones.read().clone();
    if list.is_empty() {
        map.remove(&camera_id);
    } else {
        map.insert(camera_id.clone(), list.clone());
    }
    persist(&pool, &map)
        .await
        .map_err(|e| ApiError::internal(format!("persist zones: {e}")))?;
    *zones.write() = map;
    tracing::info!(%camera_id, count = list.len(), "zones: updated");
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "zones": list, "applied": "immediate" })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_and_get_roundtrip_via_db() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.expect("migrations");
        let shared = new_shared();
        load_from_db(&pool, &shared).await;
        let zone = Zone {
            name: "yard".into(),
            kind: streaming::ai::zones::ZoneKind::Intrusion,
            points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
            dwell_secs: 5,
        };
        put_zones(
            Path("0".to_string()),
            Extension(shared.clone()),
            Extension(pool.clone()),
            Extension(AuthenticatedUser("admin".to_string())),
            axum::extract::Json(vec![zone]),
        )
        .await
        .expect("put");
        // Fresh shared map loaded from the same db sees the zone.
        let reloaded = new_shared();
        load_from_db(&pool, &reloaded).await;
        assert_eq!(reloaded.read().get("0").map(Vec::len), Some(1));

        // Invalid: polygon with 2 points.
        let bad = Zone {
            name: "bad".into(),
            kind: streaming::ai::zones::ZoneKind::Intrusion,
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            dwell_secs: 0,
        };
        let err = put_zones(
            Path("0".to_string()),
            Extension(shared.clone()),
            Extension(pool.clone()),
            Extension(AuthenticatedUser("admin".to_string())),
            axum::extract::Json(vec![bad]),
        )
        .await;
        assert!(err.is_err(), "polygon needs >= 3 points");
    }
}
