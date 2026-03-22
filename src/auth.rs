use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;

pub async fn require_auth(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    // Defense-in-depth: reject requests from browser extension origins server-side,
    // even though CORS should block them client-side. This protects against cases
    // where the browser doesn't enforce CORS (e.g. simple requests without preflight).
    if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
        if is_extension_origin(origin) {
            tracing::warn!("rejected auth from extension origin: {origin}");
            return (StatusCode::FORBIDDEN, "forbidden").into_response();
        }
    }

    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth {
        Some(value)
            if value
                .strip_prefix("Bearer ")
                .is_some_and(|t| constant_time_eq(t, &state.token)) =>
        {
            next.run(req).await
        }
        _ => {
            tracing::warn!(
                "unauthorized request from {:?}",
                req.headers()
                    .get("origin")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown")
            );
            (
                StatusCode::UNAUTHORIZED,
                [("www-authenticate", "Bearer")],
                "unauthorized",
            )
                .into_response()
        }
    }
}

/// Returns true if the origin looks like a browser extension.
fn is_extension_origin(origin: &str) -> bool {
    origin.starts_with("chrome-extension://")
        || origin.starts_with("moz-extension://")
        || origin.starts_with("safari-web-extension://")
        || origin.starts_with("extension://")
}

/// Compare two strings in constant time to prevent timing attacks.
/// XORs every byte pair and ORs results into an accumulator, so all bytes
/// are always processed regardless of where a mismatch occurs.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
