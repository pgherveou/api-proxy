mod auth;
pub mod claude;
mod gh;
mod messages;
mod pages;

use axum::Router;
use axum::routing::{any, get, post};
use tower_http::cors::CorsLayer;

pub const DEFAULT_CORS_ORIGIN: &str = "localhost, 127.0.0.1, *.github.io";

/// Patterns: "*" = any, "*.x" = suffix match, "host" = http(s)://host(:port)
pub fn cors_origin_matches(origin: &str, patterns: &str) -> bool {
    patterns.split(',').any(|pat| {
        let pat = pat.trim();
        match pat {
            "*" => true,
            p if p.starts_with("*.") => origin.ends_with(&p[1..]),
            p if p.contains("://") => origin == p,
            host => {
                for scheme in ["http://", "https://"] {
                    if let Some(rest) = origin.strip_prefix(scheme) {
                        if rest == host
                            || (rest.starts_with(host)
                                && rest.as_bytes().get(host.len()) == Some(&b':'))
                        {
                            return true;
                        }
                    }
                }
                false
            }
        }
    })
}

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
