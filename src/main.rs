mod auth;
mod claude;
mod config;
mod gh;
mod pages;

use axum::Router;
use axum::routing::{any, get, post};
use config::Config;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Clone)]
pub struct AppState {
    pub pool: claude::ClaudePool,
    pub token: String,
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
        None => {}
    }

    let token = config.token().to_string();
    tracing::info!("auth token loaded (run `api-proxy get-token` to retrieve)");

    let origin = if config.cors_origin() == "*" {
        AllowOrigin::any()
    } else {
        AllowOrigin::exact(config.cors_origin().parse().expect("invalid cors_origin"))
    };

    let cors = CorsLayer::new()
        .allow_origin(origin)
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
    };

    let public = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/", get(pages::index))
        .route("/favicon.ico", get(pages::favicon));

    let protected = Router::new()
        .route("/gh/{*path}", any(gh::handler))
        .route("/claude", post(claude::handler))
        .route("/claude/stream", post(claude::stream_handler))
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
