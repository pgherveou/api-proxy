use http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};

use api_proxy::{AppState, build_app};

fn mock_path(name: &str) -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{manifest}/tests/mocks/{name}")
}

fn make_app(cors_origin: &str, blocked_pattern: Option<&str>) -> axum::Router {
    let blocked_origin_pattern = blocked_pattern.map(|p| regex::Regex::new(p).unwrap());

    let state = AppState {
        pool: api_proxy::claude::ClaudePool::new_with_command(&[], mock_path("mock_claude.sh")),
        token: "test-token".to_string(),
        blocked_origin_pattern,
        gh_command: mock_path("mock_gh.sh"),
    };

    let cors_origin_str = cors_origin.to_string();
    let block_re = state.blocked_origin_pattern.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            let origin = origin.to_str().unwrap_or("");
            if cors_origin_str == "*" {
                return true;
            }
            if let Some(re) = &block_re {
                if re.is_match(origin) {
                    return false;
                }
            }
            if cors_origin_str.is_empty() {
                return origin.starts_with("http://localhost:")
                    || origin.starts_with("http://127.0.0.1:")
                    || origin.ends_with(".github.io");
            }
            origin == cors_origin_str
        }))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    build_app(state, cors)
}

fn claude_request_body(model: &str, stream: bool) -> String {
    serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "hello"}],
        "stream": stream,
    })
    .to_string()
}

async fn body_json(resp: http::Response<axum::body::Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn body_string(resp: http::Response<axum::body::Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn acao(resp: &http::Response<axum::body::Body>) -> Option<&str> {
    resp.headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
}

// =============================================================================
// Public routes
// =============================================================================

#[tokio::test]
async fn health_returns_ok_without_auth() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::get("/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "OK");
}

#[tokio::test]
async fn index_returns_html_without_auth() {
    let app = make_app("", None);
    let resp = app
        .oneshot(Request::get("/").body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains("<html"),
        "expected HTML response, got: {html}"
    );
}

// =============================================================================
// Auth on protected routes
// =============================================================================

#[tokio::test]
async fn auth_on_protected_routes() {
    struct Case {
        name: &'static str,
        method: &'static str,
        uri: &'static str,
        cors_origin: &'static str,
        blocked_pattern: Option<&'static str>,
        origin: Option<&'static str>,
        auth: Option<&'static str>,
        body: Option<String>,
        expect_status: StatusCode,
    }

    let cases = [
        Case {
            name: "claude rejects missing token",
            method: "POST",
            uri: "/claude/v1/messages",
            cors_origin: "",
            blocked_pattern: None,
            origin: None,
            auth: None,
            body: Some(claude_request_body("sonnet", false)),
            expect_status: StatusCode::UNAUTHORIZED,
        },
        Case {
            name: "claude rejects wrong token",
            method: "POST",
            uri: "/claude/v1/messages",
            cors_origin: "",
            blocked_pattern: None,
            origin: None,
            auth: Some("Bearer wrong-token"),
            body: Some(claude_request_body("sonnet", false)),
            expect_status: StatusCode::UNAUTHORIZED,
        },
        Case {
            name: "gh rejects missing token",
            method: "GET",
            uri: "/gh/user",
            cors_origin: "",
            blocked_pattern: None,
            origin: None,
            auth: None,
            body: None,
            expect_status: StatusCode::UNAUTHORIZED,
        },
        Case {
            name: "blocked origin returns 403 on claude",
            method: "POST",
            uri: "/claude/v1/messages",
            cors_origin: "*",
            blocked_pattern: Some("^chrome-extension://"),
            origin: Some("chrome-extension://abcdefgh"),
            auth: Some("Bearer test-token"),
            body: Some(claude_request_body("sonnet", false)),
            expect_status: StatusCode::FORBIDDEN,
        },
        Case {
            name: "blocked origin returns 403 on gh",
            method: "GET",
            uri: "/gh/user",
            cors_origin: "*",
            blocked_pattern: Some("^chrome-extension://"),
            origin: Some("chrome-extension://abcdefgh"),
            auth: Some("Bearer test-token"),
            body: None,
            expect_status: StatusCode::FORBIDDEN,
        },
        Case {
            name: "localhost origin with valid token passes auth",
            method: "GET",
            uri: "/gh/user",
            cors_origin: "",
            blocked_pattern: Some("^chrome-extension://"),
            origin: Some("http://localhost:3000"),
            auth: Some("Bearer test-token"),
            body: None,
            expect_status: StatusCode::OK,
        },
        Case {
            name: "no blocking when pattern is None",
            method: "GET",
            uri: "/gh/user",
            cors_origin: "*",
            blocked_pattern: None,
            origin: Some("chrome-extension://abcdefgh"),
            auth: Some("Bearer test-token"),
            body: None,
            expect_status: StatusCode::OK,
        },
    ];

    for case in cases {
        let app = make_app(case.cors_origin, case.blocked_pattern);
        let mut req = Request::builder()
            .method(case.method)
            .uri(case.uri)
            .header("content-type", "application/json");
        if let Some(origin) = case.origin {
            req = req.header("origin", origin);
        }
        if let Some(auth) = case.auth {
            req = req.header("authorization", auth);
        }
        let body = case
            .body
            .map(axum::body::Body::from)
            .unwrap_or(axum::body::Body::empty());
        let resp = app.oneshot(req.body(body).unwrap()).await.unwrap();
        assert_eq!(resp.status(), case.expect_status, "failed: {}", case.name);
    }
}

// =============================================================================
// CORS predicate
// =============================================================================

#[tokio::test]
async fn cors_predicate() {
    struct Case {
        name: &'static str,
        cors_origin: &'static str,
        blocked_pattern: Option<&'static str>,
        origin: &'static str,
        expect_acao: Option<&'static str>,
    }

    let cases = [
        // Default CORS (empty = localhost + *.github.io)
        Case {
            name: "default allows localhost",
            cors_origin: "",
            blocked_pattern: None,
            origin: "http://localhost:3000",
            expect_acao: Some("http://localhost:3000"),
        },
        Case {
            name: "default allows 127.0.0.1",
            cors_origin: "",
            blocked_pattern: None,
            origin: "http://127.0.0.1:8080",
            expect_acao: Some("http://127.0.0.1:8080"),
        },
        Case {
            name: "default allows github.io",
            cors_origin: "",
            blocked_pattern: None,
            origin: "https://pgherveou.github.io",
            expect_acao: Some("https://pgherveou.github.io"),
        },
        Case {
            name: "default blocks arbitrary origin",
            cors_origin: "",
            blocked_pattern: None,
            origin: "https://evil.com",
            expect_acao: None,
        },
        Case {
            name: "default blocks extension origin",
            cors_origin: "",
            blocked_pattern: Some(
                "^(chrome-extension|moz-extension|safari-web-extension|extension)://",
            ),
            origin: "chrome-extension://abcdefgh",
            expect_acao: None,
        },
        // Wildcard CORS
        Case {
            name: "wildcard allows extension origin",
            cors_origin: "*",
            blocked_pattern: None,
            origin: "chrome-extension://abcdefgh",
            expect_acao: Some("chrome-extension://abcdefgh"),
        },
        // Exact CORS origin
        Case {
            name: "exact allows matching origin",
            cors_origin: "https://myapp.com",
            blocked_pattern: None,
            origin: "https://myapp.com",
            expect_acao: Some("https://myapp.com"),
        },
        Case {
            name: "exact blocks non-matching origin",
            cors_origin: "https://myapp.com",
            blocked_pattern: None,
            origin: "https://other.com",
            expect_acao: None,
        },
    ];

    for case in cases {
        let app = make_app(case.cors_origin, case.blocked_pattern);
        let resp = app
            .oneshot(
                Request::get("/health")
                    .header(header::ORIGIN, case.origin)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(acao(&resp), case.expect_acao, "failed: {}", case.name);
    }
}

#[tokio::test]
async fn cors_on_protected_routes() {
    struct Case {
        name: &'static str,
        cors_origin: &'static str,
        blocked_pattern: Option<&'static str>,
        method: &'static str,
        uri: &'static str,
        origin: &'static str,
        expect_acao: Option<&'static str>,
    }

    let cases = [
        Case {
            name: "default allows localhost on claude",
            cors_origin: "",
            blocked_pattern: None,
            method: "POST",
            uri: "/claude/v1/messages",
            origin: "http://localhost:3000",
            expect_acao: Some("http://localhost:3000"),
        },
        Case {
            name: "default no ACAO for arbitrary origin on gh",
            cors_origin: "",
            blocked_pattern: None,
            method: "GET",
            uri: "/gh/user",
            origin: "https://evil.com",
            expect_acao: None,
        },
        Case {
            name: "exact allows matching on gh",
            cors_origin: "https://myapp.com",
            blocked_pattern: None,
            method: "GET",
            uri: "/gh/user",
            origin: "https://myapp.com",
            expect_acao: Some("https://myapp.com"),
        },
        Case {
            name: "exact blocks non-matching on gh",
            cors_origin: "https://myapp.com",
            blocked_pattern: None,
            method: "GET",
            uri: "/gh/user",
            origin: "https://other.com",
            expect_acao: None,
        },
        Case {
            name: "wildcard allows any on claude",
            cors_origin: "*",
            blocked_pattern: None,
            method: "POST",
            uri: "/claude/v1/messages",
            origin: "https://random-site.example",
            expect_acao: Some("https://random-site.example"),
        },
        Case {
            name: "github.io allowed on gh",
            cors_origin: "",
            blocked_pattern: None,
            method: "GET",
            uri: "/gh/user",
            origin: "https://pgherveou.github.io",
            expect_acao: Some("https://pgherveou.github.io"),
        },
    ];

    for case in cases {
        let app = make_app(case.cors_origin, case.blocked_pattern);
        let body = if case.method == "POST" {
            axum::body::Body::from(claude_request_body("sonnet", false))
        } else {
            axum::body::Body::empty()
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method(case.method)
                    .uri(case.uri)
                    .header("content-type", "application/json")
                    .header("origin", case.origin)
                    .header("authorization", "Bearer test-token")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(acao(&resp), case.expect_acao, "failed: {}", case.name);
    }
}

#[tokio::test]
async fn cors_preflight_on_claude() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/claude/v1/messages")
                .header("origin", "http://localhost:3000")
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "content-type,authorization",
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(acao(&resp), Some("http://localhost:3000"));
}

// =============================================================================
// Claude Messages API (buffered)
// =============================================================================

#[tokio::test]
async fn claude_buffered_response_shape() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::post("/claude/v1/messages")
                .header("content-type", "application/json")
                .header("origin", "http://localhost:3000")
                .header("authorization", "Bearer test-token")
                .body(axum::body::Body::from(claude_request_body("sonnet", false)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    assert_eq!(json["type"], "message");
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["stop_reason"], "end_turn");
    assert_eq!(json["stop_sequence"], Value::Null);
    assert_eq!(json["content"][0]["type"], "text");
    assert!(json["id"].as_str().unwrap().starts_with("msg_"));
    assert!(
        json["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("mock response")
    );
    assert_eq!(json["usage"]["input_tokens"], 10);
    assert_eq!(json["usage"]["output_tokens"], 5);
}

#[tokio::test]
async fn claude_model_handling() {
    struct Case {
        name: &'static str,
        model: &'static str,
        expect_model_echo: &'static str,
        expect_text_contains: &'static str,
    }

    let cases = [
        Case {
            name: "alias forwarded as-is",
            model: "opus",
            expect_model_echo: "opus",
            expect_text_contains: "model=opus",
        },
        Case {
            name: "full model ID mapped to alias",
            model: "claude-3-5-sonnet-20241022",
            expect_model_echo: "claude-3-5-sonnet-20241022",
            expect_text_contains: "model=sonnet",
        },
    ];

    for case in cases {
        let app = make_app("", None);
        let resp = app
            .oneshot(
                Request::post("/claude/v1/messages")
                    .header("content-type", "application/json")
                    .header("origin", "http://localhost:3000")
                    .header("authorization", "Bearer test-token")
                    .body(axum::body::Body::from(claude_request_body(
                        case.model, false,
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "failed: {}", case.name);
        let json = body_json(resp).await;
        assert_eq!(
            json["model"], case.expect_model_echo,
            "failed: {}",
            case.name
        );
        let text = json["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(case.expect_text_contains),
            "failed: {}: expected '{}' in '{text}'",
            case.name,
            case.expect_text_contains
        );
    }
}

#[tokio::test]
async fn claude_system_prompt_forwarded() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::post("/claude/v1/messages")
                .header("content-type", "application/json")
                .header("origin", "http://localhost:3000")
                .header("authorization", "Bearer test-token")
                .body(axum::body::Body::from(
                    serde_json::json!({
                        "model": "sonnet",
                        "max_tokens": 1024,
                        "messages": [{"role": "user", "content": "hi"}],
                        "system": "you are a pirate",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let text = json["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("system=you are a pirate"),
        "expected system prompt in response, got: {text}"
    );
}

// =============================================================================
// Claude Messages API (streaming)
// =============================================================================

#[tokio::test]
async fn claude_streaming_returns_sse_events() {
    let app = make_app("", None);
    let resp = app
        .oneshot(
            Request::post("/claude/v1/messages")
                .header("content-type", "application/json")
                .header("origin", "http://localhost:3000")
                .header("authorization", "Bearer test-token")
                .body(axum::body::Body::from(claude_request_body("sonnet", true)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    for event_type in [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "content_block_stop",
        "message_delta",
        "message_stop",
    ] {
        assert!(
            body.contains(&format!("event: {event_type}")),
            "missing {event_type}"
        );
    }
    assert!(
        body.contains("mock response"),
        "missing mock response in SSE body"
    );
}

// =============================================================================
// GitHub API proxy
// =============================================================================

#[tokio::test]
async fn gh_proxy() {
    struct Case {
        name: &'static str,
        method: &'static str,
        uri: &'static str,
        accept: Option<&'static str>,
        body: Option<&'static str>,
        expect_path: &'static str,
        expect_method: &'static str,
    }

    let cases = [
        Case {
            name: "GET proxied",
            method: "GET",
            uri: "/gh/user",
            accept: None,
            body: None,
            expect_path: "user",
            expect_method: "GET",
        },
        Case {
            name: "query params preserved",
            method: "GET",
            uri: "/gh/repos/owner/repo/issues?state=open&per_page=5",
            accept: None,
            body: None,
            expect_path: "repos/owner/repo/issues?state=open&per_page=5",
            expect_method: "GET",
        },
        Case {
            name: "POST with body",
            method: "POST",
            uri: "/gh/graphql",
            accept: None,
            body: Some(r#"{"query":"{ viewer { login } }"}"#),
            expect_path: "graphql",
            expect_method: "POST",
        },
        Case {
            name: "accept header forwarded",
            method: "GET",
            uri: "/gh/user",
            accept: Some("application/vnd.github.v3+json"),
            body: None,
            expect_path: "user",
            expect_method: "GET",
        },
    ];

    for case in cases {
        let app = make_app("", None);
        let mut req = Request::builder()
            .method(case.method)
            .uri(case.uri)
            .header("origin", "http://localhost:3000")
            .header("authorization", "Bearer test-token");
        if let Some(accept) = case.accept {
            req = req.header("accept", accept);
        }
        let body = case
            .body
            .map(|b| axum::body::Body::from(b.to_string()))
            .unwrap_or(axum::body::Body::empty());
        let resp = app.oneshot(req.body(body).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "failed: {}", case.name);
        let json = body_json(resp).await;
        assert_eq!(json["path"], case.expect_path, "failed: {}", case.name);
        assert_eq!(json["method"], case.expect_method, "failed: {}", case.name);

        if let Some(req_body) = case.body {
            let echoed = json["body"].as_str().unwrap();
            assert!(
                echoed.contains(&req_body[1..20]),
                "failed: {}: body not forwarded",
                case.name
            );
        }
        if let Some(accept) = case.accept {
            assert_eq!(
                json["accept"],
                format!("Accept: {accept}"),
                "failed: {}",
                case.name
            );
        }
    }
}
