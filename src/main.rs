mod auth;
mod claude;
mod config;
mod gh;
mod messages;
mod pages;

use axum::Router;
use axum::routing::{any, get, post};
use config::Config;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Clone)]
pub struct AppState {
    pub pool: claude::ClaudePool,
    pub token: String,
    /// Compiled regex for origins to block; None means no blocking.
    pub blocked_origin_pattern: Option<regex::Regex>,
}

impl axum::extract::FromRef<AppState> for claude::ClaudePool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let mut config = Config::load();

    match config.command {
        Some(config::Command::GetToken) => {
            print!("{}", config.token());
            return;
        }
        Some(config::Command::RegenerateToken) => {
            config.regenerate_token();
            print!("{}", config.token());
            return;
        }
        Some(config::Command::ShowConfig) => {
            config.show();
            return;
        }
        Some(config::Command::SetCorsOrigin { ref origin }) => {
            config.set_cors_origin(origin.clone());
            println!("Saved. Restart the service for changes to take effect.");
            return;
        }
        Some(config::Command::SetBlockedOriginPattern { ref pattern }) => {
            config.set_blocked_origin_pattern(pattern.clone());
            println!("Saved. Restart the service for changes to take effect.");
            return;
        }
        None => {}
    }

    let token = config.token().to_string();
    tracing::info!("auth token loaded (run `api-proxy get-token` to retrieve)");

    let blocked_origin_pattern = config
        .blocked_origin_pattern()
        .map(|p| regex::Regex::new(p).expect("invalid blocked_origin_pattern"));

    let cors_origin = config.cors_origin().to_string();
    let block_re = blocked_origin_pattern.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            let origin = origin.to_str().unwrap_or("");
            if cors_origin == "*" {
                return true;
            }
            if let Some(re) = &block_re {
                if re.is_match(origin) {
                    tracing::warn!("blocked request from origin: {origin}");
                    return false;
                }
            }
            if cors_origin.is_empty() {
                return origin.starts_with("http://localhost:")
                    || origin.starts_with("http://127.0.0.1:")
                    || origin.ends_with(".github.io");
            }
            origin == cors_origin
        }))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    let pool_size = config.claude_pool_size();
    let state = AppState {
        pool: claude::ClaudePool::new(&[
            ("", pool_size),
            ("sonnet", pool_size),
            ("haiku", pool_size),
            ("opus", pool_size),
        ]),
        token,
        blocked_origin_pattern,
    };

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

    let app = public.merge(protected).with_state(state).layer(cors);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port()));
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
