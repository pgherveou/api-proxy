mod auth;
pub mod claude;
mod gh;
mod messages;
mod pages;

use axum::Router;
use axum::routing::{any, get, post};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: claude::ClaudePool,
    pub token: String,
    /// Compiled regex for origins to block; None means no blocking.
    pub blocked_origin_pattern: Option<regex::Regex>,
    /// Command to run for `gh` CLI (default: "gh").
    pub gh_command: String,
}

impl axum::extract::FromRef<AppState> for claude::ClaudePool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

pub fn build_app(state: AppState, cors: CorsLayer) -> Router {
    let public = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/", get(pages::index))
        .route("/favicon.ico", get(pages::favicon));

    let protected = Router::new()
        .route("/gh/{*path}", any(gh::handler))
        .route("/claude/v1/messages", post(messages::handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    public.merge(protected).with_state(state).layer(cors)
}
