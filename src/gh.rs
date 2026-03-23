use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::process::Command;

use crate::AppState;

pub async fn handler(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().strip_prefix("/gh/").unwrap_or("");
    let query = req.uri().query();

    let api_path = match query {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    };

    let accept = req
        .headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let body = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();

    let has_body = !body.is_empty();

    let mut cmd = Command::new(&state.gh_command);
    cmd.arg("api").arg(&api_path);
    cmd.arg("--method").arg(method.as_str());

    if let Some(accept) = &accept {
        cmd.arg("-H").arg(format!("Accept: {accept}"));
    }

    if has_body {
        cmd.arg("--input").arg("-");
        cmd.stdin(std::process::Stdio::piped());
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let result = spawn_with_stdin(&mut cmd, if has_body { Some(&body) } else { None }).await;

    let content_type = accept.as_deref().unwrap_or("application/json");

    match result {
        Ok((status, stdout, stderr)) => {
            if status.success() {
                (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], stdout).into_response()
            } else {
                tracing::warn!("gh api failed: {}", String::from_utf8_lossy(&stderr));
                (StatusCode::BAD_GATEWAY, "gh api request failed").into_response()
            }
        }
        Err(e) => {
            tracing::error!("failed to run gh: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn spawn_with_stdin(
    cmd: &mut Command,
    stdin_data: Option<&Bytes>,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = cmd.spawn()?;

    if let Some(data) = stdin_data {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data).await?;
            drop(stdin);
        }
    }

    let output = child.wait_with_output().await?;
    Ok((output.status, output.stdout, output.stderr))
}
