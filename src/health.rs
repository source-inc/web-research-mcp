// crates/web-research-mcp/src/health.rs
use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;

use crate::metrics::Metrics;

pub struct HealthState {
    pub host: String,
    pub metrics: Metrics,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub host: String,
}

pub fn router(state: Arc<HealthState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn healthz(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        service: "web-research-mcp",
        host: state.host.clone(),
    })
}

async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "web-research-mcp",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn metrics(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    match state.metrics.encode() {
        Ok(body) => (axum::http::StatusCode::OK, body),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("metrics error: {e}"),
        ),
    }
}
