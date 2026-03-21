use std::collections::HashMap;
use std::convert::Infallible;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct ClaudeRequest {
    prompt: String,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fallback_model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
}

impl ClaudeRequest {
    /// Args that differ from model-only pool args (effort, fallback, system prompt).
    fn extra_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(ref effort) = self.effort {
            args.extend(["--effort".into(), effort.clone()]);
        }
        if let Some(ref fallback) = self.fallback_model {
            args.extend(["--fallback-model".into(), fallback.clone()]);
        }
        if let Some(ref sp) = self.system_prompt {
            args.extend(["--system-prompt".into(), sp.clone()]);
        }
        args
    }

    /// The model key for pool lookup (None = default model).
    fn model_key(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Full CLI args for a dedicated (non-pool) spawn.
    fn all_args(&self) -> Vec<String> {
        let mut args = self.extra_args();
        if let Some(ref model) = self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args
    }
}

#[derive(Serialize)]
pub struct ClaudeResponse {
    response: String,
}

// The verbose stream-json protocol emits three kinds of messages:
// - stream_event: wraps raw API events (content_block_delta, message_stop, etc.)
// - result: final result with full text (signals end of response)
// - system/assistant/rate_limit_event: ignored
#[derive(Deserialize)]
#[serde(tag = "type")]
enum StreamMessage {
    #[serde(rename = "stream_event")]
    StreamEvent { event: StreamEvent },
    #[serde(rename = "result")]
    Result {
        result: String,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum StreamEvent {
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: Delta },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Delta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(other)]
    Other,
}

struct ClaudeProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl ClaudeProcess {
    fn spawn(extra_args: &[String]) -> std::io::Result<Self> {
        let mut cmd = Command::new("claude");
        cmd.args([
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
        ]);
        cmd.args(extra_args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        tracing::debug!(pid, ?extra_args, "spawned claude process");
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(ClaudeProcess { child, stdout })
    }

    fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn write_prompt(&mut self, prompt: &str) -> Result<(), String> {
        let stdin = self.child.stdin.as_mut().ok_or("stdin closed")?;
        let msg = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prompt }
        });
        let mut line = serde_json::to_string(&msg).unwrap();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    /// Read the next parsed message from stdout, skipping unparseable lines.
    async fn next_message(&mut self, buf: &mut String) -> Result<StreamMessage, String> {
        loop {
            buf.clear();
            let n = self
                .stdout
                .read_line(buf)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("claude process exited".into());
            }
            if let Ok(msg) = serde_json::from_str::<StreamMessage>(buf) {
                if !matches!(msg, StreamMessage::Other) {
                    return Ok(msg);
                }
            }
        }
    }

    /// Buffered: collect entire response
    async fn recv(&mut self) -> Result<String, String> {
        let pid = self.pid();
        let start = Instant::now();
        let mut text = String::with_capacity(4096);
        let mut buf = String::with_capacity(512);
        let mut first_token = true;

        loop {
            let msg = self.next_message(&mut buf).await?;
            match msg {
                StreamMessage::StreamEvent { event } => match event {
                    StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text: t },
                    } => {
                        if first_token {
                            tracing::debug!(
                                pid,
                                ttfb_ms = start.elapsed().as_millis() as u64,
                                "first token received"
                            );
                            first_token = false;
                        }
                        text.push_str(&t);
                    }
                    StreamEvent::MessageStop => {
                        tracing::debug!(
                            pid,
                            duration_ms = start.elapsed().as_millis() as u64,
                            response_len = text.len(),
                            "message complete"
                        );
                        return Ok(text);
                    }
                    _ => {}
                },
                StreamMessage::Result { result, is_error } => {
                    if is_error {
                        return Err(result);
                    }
                    return Ok(if text.is_empty() { result } else { text });
                }
                StreamMessage::Other => {}
            }
        }
    }
}

struct ModelPool {
    warm: Mutex<Vec<ClaudeProcess>>,
    args: Vec<String>,
    target: usize,
    replenish: Notify,
}

#[derive(Clone)]
pub struct ClaudePool {
    pools: Arc<HashMap<String, ModelPool>>,
    next_req_id: Arc<AtomicU64>,
}

impl ClaudePool {
    pub fn new(models: &[(&str, usize)]) -> Self {
        let mut pools = HashMap::new();
        for &(model, size) in models {
            let args = if model.is_empty() {
                vec![]
            } else {
                vec!["--model".into(), model.into()]
            };
            pools.insert(
                model.to_string(),
                ModelPool {
                    warm: Mutex::new(Vec::with_capacity(size)),
                    args,
                    target: size,
                    replenish: Notify::new(),
                },
            );
        }

        let pool = ClaudePool {
            pools: Arc::new(pools),
            next_req_id: Arc::new(AtomicU64::new(1)),
        };

        // Spawn a replenisher task per model
        for (model, _) in pool.pools.iter() {
            let pools = Arc::clone(&pool.pools);
            let model = model.clone();
            tokio::spawn(async move {
                let mp = pools.get(&model).unwrap();
                fill_pool(&mp.warm, &mp.args, mp.target).await;
                let label = if model.is_empty() { "default" } else { &model };
                tracing::info!("claude pool ready: {label} ({} processes)", mp.target);

                loop {
                    mp.replenish.notified().await;
                    fill_pool(&mp.warm, &mp.args, mp.target).await;
                }
            });
        }

        pool
    }

    fn next_req_id(&self) -> u64 {
        self.next_req_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn take(&self, req: &ClaudeRequest) -> Result<(ClaudeProcess, &'static str), String> {
        let has_extra = !req.extra_args().is_empty();

        // If there are extra args (effort, system_prompt, etc.) we must spawn a dedicated process
        if !has_extra {
            let key = req.model_key().unwrap_or("");
            if let Some(mp) = self.pools.get(key) {
                let mut warm = mp.warm.lock().await;
                while let Some(mut proc) = warm.pop() {
                    if proc.is_alive() {
                        let remaining = warm.len();
                        drop(warm);
                        mp.replenish.notify_one();
                        tracing::debug!(pid = proc.pid(), model = key, remaining, "took from pool");
                        return Ok((proc, "pool"));
                    }
                    tracing::debug!(pid = proc.pid(), "discarded dead process from pool");
                }
                drop(warm);
                mp.replenish.notify_one();
                tracing::warn!(model = key, "claude pool exhausted, spawning on-demand");
                let proc = ClaudeProcess::spawn(&mp.args).map_err(|e| e.to_string())?;
                return Ok((proc, "on-demand"));
            }
        }

        // No pool for this model or has extra args: spawn dedicated process
        let proc = ClaudeProcess::spawn(&req.all_args()).map_err(|e| e.to_string())?;
        Ok((proc, "custom-args"))
    }
}

async fn fill_pool(warm: &Mutex<Vec<ClaudeProcess>>, args: &[String], target: usize) {
    let current = warm.lock().await.len();
    let needed = target.saturating_sub(current);
    if needed == 0 {
        return;
    }

    let handles: Vec<_> = (0..needed)
        .map(|_| {
            let args = args.to_vec();
            tokio::spawn(async move { ClaudeProcess::spawn(&args) })
        })
        .collect();

    for handle in handles {
        match handle.await {
            Ok(Ok(proc)) => warm.lock().await.push(proc),
            Ok(Err(e)) => tracing::error!("failed to spawn claude process: {e}"),
            Err(e) => tracing::error!("spawn task panicked: {e}"),
        }
    }
}

/// Buffered handler: collects full response then returns JSON
pub async fn handler(
    axum::extract::State(pool): axum::extract::State<ClaudePool>,
    Json(req): Json<ClaudeRequest>,
) -> Response {
    let req_id = pool.next_req_id();
    tracing::info!(
        req_id,
        prompt_len = req.prompt.len(),
        model = req.model.as_deref().unwrap_or("default"),
        "claude request start",
    );
    tracing::debug!(req_id, prompt_preview = %req.prompt.chars().take(80).collect::<String>());
    let request_start = Instant::now();

    let (mut proc, source) = match pool.take(&req).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(req_id, "failed to spawn claude: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    let pid = proc.pid();
    tracing::info!(req_id, pid, source, "assigned claude process");

    if let Err(e) = proc.write_prompt(&req.prompt).await {
        tracing::warn!(req_id, pid, "failed to write prompt: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    let result = tokio::time::timeout(REQUEST_TIMEOUT, proc.recv()).await;
    match result {
        Ok(Ok(response)) => {
            tracing::info!(
                req_id,
                pid,
                duration_ms = request_start.elapsed().as_millis() as u64,
                response_len = response.len(),
                "claude request complete"
            );
            Json(ClaudeResponse { response }).into_response()
        }
        Ok(Err(e)) => {
            tracing::warn!(
                req_id,
                pid,
                duration_ms = request_start.elapsed().as_millis() as u64,
                "claude request failed: {e}"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
        Err(_) => {
            tracing::warn!(
                req_id,
                pid,
                "claude request timed out after {}s",
                REQUEST_TIMEOUT.as_secs()
            );
            (StatusCode::GATEWAY_TIMEOUT, "request timed out").into_response()
        }
    }
}

/// SSE streaming handler: streams tokens as they arrive.
/// With `Accept: text/plain`, returns raw text (ideal for curl demos).
pub async fn stream_handler(
    axum::extract::State(pool): axum::extract::State<ClaudePool>,
    headers: HeaderMap,
    Json(req): Json<ClaudeRequest>,
) -> Response {
    let req_id = pool.next_req_id();
    tracing::info!(
        req_id,
        prompt_len = req.prompt.len(),
        model = req.model.as_deref().unwrap_or("default"),
        "claude stream start",
    );
    tracing::debug!(req_id, prompt_preview = %req.prompt.chars().take(80).collect::<String>());

    let (mut proc, source) = match pool.take(&req).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(req_id, "failed to spawn claude: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    let pid = proc.pid();
    tracing::info!(req_id, pid, source, "assigned claude process (stream)");

    if let Err(e) = proc.write_prompt(&req.prompt).await {
        tracing::warn!(req_id, pid, "failed to write prompt: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    let plain_text = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/plain"));

    if plain_text {
        let stream = text_stream(proc, req_id, pid);
        let body = Body::from_stream(stream);
        Response::builder()
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(body)
            .unwrap()
            .into_response()
    } else {
        let stream = token_stream(proc, req_id, pid);
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    }
}

fn token_stream(
    mut proc: ClaudeProcess,
    req_id: u64,
    pid: u32,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let start = Instant::now();
    let mut first_token = true;
    let mut total_len: usize = 0;

    async_stream::stream! {
        let mut buf = String::with_capacity(512);
        loop {
            let read_fut = proc.next_message(&mut buf);
            let msg = match tokio::time::timeout(REQUEST_TIMEOUT, read_fut).await {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => {
                    tracing::warn!(req_id, pid, "stream read error: {e}");
                    yield Ok(Event::default().event("error").data(e.to_string()));
                    break;
                }
                Err(_) => {
                    tracing::warn!(req_id, pid, "stream timed out after {}s", REQUEST_TIMEOUT.as_secs());
                    yield Ok(Event::default().event("error").data("request timed out"));
                    break;
                }
            };
            match msg {
                StreamMessage::StreamEvent { event } => match event {
                    StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text },
                    } => {
                        if first_token {
                            tracing::debug!(
                                req_id,
                                pid,
                                ttfb_ms = start.elapsed().as_millis() as u64,
                                "first token (stream)"
                            );
                            first_token = false;
                        }
                        total_len += text.len();
                        yield Ok(Event::default().data(&text));
                    }
                    StreamEvent::MessageStop => {
                        tracing::info!(
                            req_id,
                            pid,
                            duration_ms = start.elapsed().as_millis() as u64,
                            response_len = total_len,
                            "stream complete"
                        );
                        yield Ok(Event::default().event("done").data("[DONE]"));
                        break;
                    }
                    _ => {}
                },
                StreamMessage::Result { result, is_error } => {
                    tracing::info!(
                        req_id,
                        pid,
                        duration_ms = start.elapsed().as_millis() as u64,
                        response_len = total_len,
                        is_error,
                        "stream complete (result)"
                    );
                    if is_error {
                        yield Ok(Event::default().event("error").data(result));
                    } else {
                        // If no text_delta events were received, emit the result text
                        if total_len == 0 && !result.is_empty() {
                            tracing::warn!(req_id, pid, "no stream deltas received, falling back to result text");
                            yield Ok(Event::default().data(&result));
                        }
                        yield Ok(Event::default().event("done").data("[DONE]"));
                    }
                    break;
                }
                StreamMessage::Other => {}
            }
        }
    }
}

fn text_stream(
    mut proc: ClaudeProcess,
    req_id: u64,
    pid: u32,
) -> impl Stream<Item = Result<String, Infallible>> {
    let start = Instant::now();
    let mut first_token = true;
    let mut total_len: usize = 0;

    async_stream::stream! {
        let mut buf = String::with_capacity(512);
        loop {
            let read_fut = proc.next_message(&mut buf);
            let msg = match tokio::time::timeout(REQUEST_TIMEOUT, read_fut).await {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => {
                    tracing::warn!(req_id, pid, "stream read error: {e}");
                    if total_len == 0 {
                        yield Ok(format!("Error: {e}"));
                    }
                    break;
                }
                Err(_) => {
                    tracing::warn!(req_id, pid, "stream timed out after {}s", REQUEST_TIMEOUT.as_secs());
                    if total_len == 0 {
                        yield Ok("Error: request timed out".into());
                    }
                    break;
                }
            };
            match msg {
                StreamMessage::StreamEvent { event } => match event {
                    StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text },
                    } => {
                        if first_token {
                            tracing::debug!(
                                req_id, pid,
                                ttfb_ms = start.elapsed().as_millis() as u64,
                                "first token (text stream)"
                            );
                            first_token = false;
                        }
                        total_len += text.len();
                        yield Ok(text);
                    }
                    StreamEvent::MessageStop => {
                        tracing::info!(
                            req_id, pid,
                            duration_ms = start.elapsed().as_millis() as u64,
                            response_len = total_len,
                            "text stream complete"
                        );
                        break;
                    }
                    _ => {}
                },
                StreamMessage::Result { result, is_error } => {
                    tracing::info!(
                        req_id, pid,
                        duration_ms = start.elapsed().as_millis() as u64,
                        response_len = total_len,
                        is_error,
                        "text stream complete (result)"
                    );
                    if is_error {
                        yield Ok(format!("Error: {result}"));
                    } else if total_len == 0 && !result.is_empty() {
                        yield Ok(result);
                    }
                    break;
                }
                StreamMessage::Other => {}
            }
        }
        yield Ok("\n".into());
    }
}
