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
    if let Some(re) = &state.blocked_origin_pattern {
        if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
            if re.is_match(origin) {
                tracing::warn!("rejected request from blocked origin: {origin}");
                return (StatusCode::FORBIDDEN, "forbidden").into_response();
            }
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
