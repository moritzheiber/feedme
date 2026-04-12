use std::sync::Arc;

use sqlx::SqlitePool;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};

pub mod auth;
pub mod fever;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub api_key: String,
}

impl AppState {
    pub fn new(pool: SqlitePool, api_key: String) -> Arc<Self> {
        Arc::new(Self { pool, api_key })
    }
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route(
            "/",
            axum::routing::get(fever::discovery).post(fever::handler),
        )
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_request(DefaultOnRequest::new())
                .on_response(DefaultOnResponse::new()),
        )
        .with_state(state)
}
