//! `GET /api/tools` — the tool/skill registry listing (SPEC §3.5,
//! capability `tools`). The transparency surface: everything exposed to
//! the model, built-ins and MCP plugin servers alike.

use crate::agent::ToolRegistry;
use axum::Json;
use axum::extract::Extension;
use serde_json::json;
use std::sync::Arc;

#[tracing::instrument(skip_all)]
pub async fn list_tools(
    Extension(registry): Extension<Arc<ToolRegistry>>,
    Extension(_user): Extension<security::middleware::AuthenticatedUser>,
) -> Json<serde_json::Value> {
    let tools = registry.cached_specs();
    Json(json!({ "tools": tools }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentConfig;
    use crate::stream_manager::StreamManager;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::StatusCode;
    use security::middleware::AuthenticatedUser;
    use tower::ServiceExt;

    #[tokio::test]
    async fn tools_endpoint_lists_builtins_with_sources() {
        let shared = Arc::new(std::sync::RwLock::new(streaming::tools::ToolsConfig {
            weather_enabled: true,
            weather_city: "Guangzhou".into(),
            timeout_secs: 2,
        }));
        let registry = Arc::new(ToolRegistry::new(shared, &AgentConfig::default()));
        registry.attach_streams(Arc::new(StreamManager::new()));
        let app = axum::Router::new()
            .route("/api/tools", axum::routing::get(list_tools))
            .layer(Extension(registry))
            .layer(Extension(AuthenticatedUser("tester".to_string())));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/tools")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tools = json["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"time.now"));
        assert!(names.contains(&"weather.current"));
        assert!(names.contains(&"camera.snapshot"));
        assert!(
            tools
                .iter()
                .all(|t| t["source"].as_str() == Some("builtin"))
        );
        assert!(
            tools
                .iter()
                .all(|t| t["input_schema"]["type"] == serde_json::json!("object"))
        );
    }
}
