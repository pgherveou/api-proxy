use std::convert::Infallible;
use std::time::Instant;

use axum::Json;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudePool, Delta, REQUEST_TIMEOUT, RecvResult, StreamEvent, StreamMessage};

// -- Request types --

#[derive(Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[allow(dead_code)]
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
    // Accepted but ignored (no CLI equivalent)
    #[serde(default)]
    #[allow(dead_code)]
    pub temperature: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub top_p: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub top_k: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlockInput>),
}

#[derive(Deserialize)]
pub struct ContentBlockInput {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub text: Option<String>,
}

// -- Response types --

#[derive(Serialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: UsageResponse,
}

#[derive(Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub type_: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct UsageResponse {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

// -- Model mapping --

fn map_model(model: &str) -> &str {
    // Short aliases pass through
    match model {
        "haiku" | "sonnet" | "opus" => return model,
        _ => {}
    }
    // Map full Anthropic model IDs to CLI aliases
    if model.contains("haiku") {
        "haiku"
    } else if model.contains("opus") {
        "opus"
    } else if model.contains("sonnet") {
        "sonnet"
    } else {
        model
    }
}

// -- Message flattening --

fn flatten_messages(messages: &[Message]) -> String {
    if messages.len() == 1 {
        return extract_text(&messages[0]);
    }

    let mut parts = Vec::new();
    for msg in messages {
        let text = extract_text(msg);
        match msg.role.as_str() {
            "assistant" => {
                parts.push(format!("<previous_response>\n{text}\n</previous_response>"));
            }
            _ => parts.push(text),
        }
    }
    parts.join("\n\n")
}

fn extract_text(msg: &Message) -> String {
    match &msg.content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter(|b| b.type_ == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join(""),
    }
}

// -- Handler --

pub async fn handler(
    axum::extract::State(pool): axum::extract::State<ClaudePool>,
    Json(req): Json<MessagesRequest>,
) -> Response {
    let stream = req.stream.unwrap_or(false);
    let req_id = pool.next_req_id();
    let model_alias = map_model(&req.model);
    let prompt = flatten_messages(&req.messages);
    let model_echo = req.model.clone();

    let mut extra_args = Vec::new();
    if let Some(ref system) = req.system {
        extra_args.extend(["--system-prompt".into(), system.clone()]);
    }

    tracing::info!(
        req_id,
        prompt_len = prompt.len(),
        model = model_alias,
        stream,
        "messages request start",
    );

    let (mut proc, source) = match pool.take(Some(model_alias), &extra_args).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(req_id, "failed to spawn claude: {e}");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e);
        }
    };

    let pid = proc.pid();
    tracing::info!(req_id, pid, source, "assigned claude process");

    if let Err(e) = proc.write_prompt(&prompt).await {
        tracing::warn!(req_id, pid, "failed to write prompt: {e}");
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }

    if stream {
        let stream = messages_stream(proc, req_id, pid, model_echo);
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    } else {
        buffered_response(proc, req_id, pid, model_echo).await
    }
}

async fn buffered_response(
    mut proc: crate::claude::ClaudeProcess,
    req_id: u64,
    pid: u32,
    model: String,
) -> Response {
    let request_start = Instant::now();
    let result = tokio::time::timeout(REQUEST_TIMEOUT, proc.recv()).await;

    match result {
        Ok(Ok(RecvResult { text, usage })) => {
            tracing::info!(
                req_id,
                pid,
                duration_ms = request_start.elapsed().as_millis() as u64,
                response_len = text.len(),
                "messages request complete"
            );
            Json(MessagesResponse {
                id: format!("msg_{req_id:08x}"),
                type_: "message".into(),
                role: "assistant".into(),
                content: vec![ContentBlock {
                    type_: "text".into(),
                    text,
                }],
                model,
                stop_reason: Some("end_turn".into()),
                stop_sequence: None,
                usage: UsageResponse {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                },
            })
            .into_response()
        }
        Ok(Err(e)) => {
            tracing::warn!(req_id, pid, "messages request failed: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
        Err(_) => {
            tracing::warn!(req_id, pid, "messages request timed out");
            error_response(StatusCode::GATEWAY_TIMEOUT, "request timed out")
        }
    }
}

fn messages_stream(
    mut proc: crate::claude::ClaudeProcess,
    req_id: u64,
    pid: u32,
    model: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let start = Instant::now();
    let mut first_token = true;
    let mut total_len: usize = 0;
    let mut closed = false;

    async_stream::stream! {
        // Emit message_start
        yield Ok(sse_event("message_start", &serde_json::json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_{req_id:08x}"),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        })));

        // Emit content_block_start
        yield Ok(sse_event("content_block_start", &serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        })));

        let mut buf = String::with_capacity(512);
        loop {
            let read_fut = proc.next_message(&mut buf);
            let msg = match tokio::time::timeout(REQUEST_TIMEOUT, read_fut).await {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => {
                    tracing::warn!(req_id, pid, "stream read error: {e}");
                    yield Ok(sse_event("error", &serde_json::json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": e.to_string() }
                    })));
                    closed = true;
                    break;
                }
                Err(_) => {
                    tracing::warn!(req_id, pid, "stream timed out");
                    yield Ok(sse_event("error", &serde_json::json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": "request timed out" }
                    })));
                    closed = true;
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
                                "first token (stream)"
                            );
                            first_token = false;
                        }
                        total_len += text.len();
                        yield Ok(sse_event("content_block_delta", &serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": { "type": "text_delta", "text": text }
                        })));
                    }
                    StreamEvent::MessageStop => {
                        break;
                    }
                    _ => {}
                },
                StreamMessage::Result { result, is_error, usage } => {
                    if is_error {
                        yield Ok(sse_event("error", &serde_json::json!({
                            "type": "error",
                            "error": { "type": "api_error", "message": result }
                        })));
                        closed = true;
                        break;
                    }
                    // Fallback: if no deltas were received, emit the result as a delta
                    if total_len == 0 && !result.is_empty() {
                        yield Ok(sse_event("content_block_delta", &serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": { "type": "text_delta", "text": result }
                        })));
                        total_len = result.len();
                    }
                    // Emit closing events with usage
                    let u = usage.unwrap_or_default();
                    yield Ok(sse_event("content_block_stop", &serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0
                    })));
                    yield Ok(sse_event("message_delta", &serde_json::json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                        "usage": { "output_tokens": u.output_tokens }
                    })));
                    yield Ok(sse_event("message_stop", &serde_json::json!({
                        "type": "message_stop"
                    })));
                    closed = true;
                    tracing::info!(
                        req_id, pid,
                        duration_ms = start.elapsed().as_millis() as u64,
                        response_len = total_len,
                        "stream complete (result)"
                    );
                    break;
                }
                StreamMessage::Other => {}
            }
        }

        // Emit closing sequence if we broke out via MessageStop (not Result)
        if !closed {
            yield Ok(sse_event("content_block_stop", &serde_json::json!({
                "type": "content_block_stop",
                "index": 0
            })));
            yield Ok(sse_event("message_delta", &serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                "usage": { "output_tokens": 0 }
            })));
            yield Ok(sse_event("message_stop", &serde_json::json!({
                "type": "message_stop"
            })));
            tracing::info!(
                req_id, pid,
                duration_ms = start.elapsed().as_millis() as u64,
                response_len = total_len,
                "stream complete"
            );
        }
    }
}

fn sse_event(event_type: &str, data: &serde_json::Value) -> Event {
    Event::default()
        .event(event_type)
        .data(serde_json::to_string(data).unwrap())
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": message
            }
        })),
    )
        .into_response()
}
