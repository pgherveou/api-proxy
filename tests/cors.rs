use http::{Request, StatusCode, header};
use tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};

use api_proxy::{AppState, build_app};

fn make_app(cors_origin: &str, blocked_pattern: Option<&str>) -> axum::Router {
    let token = "test-token".to_string();

    let blocked_origin_pattern = blocked_pattern
        .map(|p| regex::Regex::new(p).unwrap());

    let state = AppState {
        pool: api_proxy::claude::ClaudePool::new(&[]),
        token,
        blocked_origin_pattern,
    };

    let cors_origin = cors_origin.to_string();
    let block_re = state.blocked_origin_pattern.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            let origin = origin.to_str().unwrap_or("");
            if cors_origin == "*" {
                return true;
            }
            if let Some(re) = &block_re {
                if re.is_match(origin) {
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

    build_app(state, cors)
}

fn acao(resp: &http::Response<axum::body::Body>) -> Option<&str> {
    resp.headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
}

// --- Default CORS (empty = localhost + *.github.io) ---

#[tokio::test]
async fn cors_default_allows_localhost() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "http://localhost:3000")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(acao(&resp), Some("http://localhost:3000"));
}

#[tokio::test]
async fn cors_default_allows_127() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "http://127.0.0.1:8080")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(acao(&resp), Some("http://127.0.0.1:8080"));
}

#[tokio::test]
async fn cors_default_allows_github_io() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "https://pgherveou.github.io")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(acao(&resp), Some("https://pgherveou.github.io"));
}

#[tokio::test]
async fn cors_default_blocks_arbitrary_origin() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "https://evil.com")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acao(&resp), None);
}

#[tokio::test]
async fn cors_default_blocks_extension_origin() {
    let app = make_app("", Some("^(chrome-extension|moz-extension|safari-web-extension|extension)://"));
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "chrome-extension://abcdefgh")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acao(&resp), None);
}

// --- Wildcard CORS ---

#[tokio::test]
async fn cors_wildcard_allows_extension_origin() {
    let app = make_app("*", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "chrome-extension://abcdefgh")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // predicate-based CORS echoes back the request origin rather than "*"
    assert_eq!(acao(&resp), Some("chrome-extension://abcdefgh"));
}

// --- Exact CORS origin ---

#[tokio::test]
async fn cors_exact_allows_matching_origin() {
    let app = make_app("https://myapp.com", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "https://myapp.com")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acao(&resp), Some("https://myapp.com"));
}

#[tokio::test]
async fn cors_exact_blocks_other_origin() {
    let app = make_app("https://myapp.com", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .header(header::ORIGIN, "https://other.com")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acao(&resp), None);
}

// --- Auth middleware: blocked origins return 403 ---

#[tokio::test]
async fn auth_blocks_extension_origin_with_valid_token() {
    let app = make_app("", Some("^chrome-extension://"));
    let resp = app
        .oneshot(
            Request::get("/gh/user")
                .header(header::ORIGIN, "chrome-extension://abcdefgh")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn auth_allows_localhost_origin_with_valid_token() {
    let app = make_app("", Some("^chrome-extension://"));
    let resp = app
        .oneshot(
            Request::get("/gh/user")
                .header(header::ORIGIN, "http://localhost:3000")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Auth passes; gh handler may fail but not with 401/403
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn auth_no_blocking_when_pattern_empty() {
    let app = make_app("*", None); // no blocked pattern
    let resp = app
        .oneshot(
            Request::get("/gh/user")
                .header(header::ORIGIN, "chrome-extension://abcdefgh")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn auth_rejects_missing_token() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/gh/user")
                .header(header::ORIGIN, "http://localhost:3000")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
