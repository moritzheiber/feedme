use std::sync::Arc;

use sqlx::SqlitePool;

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
        .with_state(state)
}
