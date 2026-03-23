use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

// The verbose stream-json protocol emits three kinds of messages:
// - stream_event: wraps raw API events (content_block_delta, message_stop, etc.)
// - result: final result with full text (signals end of response)
// - system/assistant/rate_limit_event: ignored
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum StreamMessage {
    #[serde(rename = "stream_event")]
    StreamEvent { event: StreamEvent },
    #[serde(rename = "result")]
    Result {
        result: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        usage: Option<Usage>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: Delta },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum Delta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Clone, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

pub struct RecvResult {
    pub text: String,
    pub usage: Usage,
}

pub struct ClaudeProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl ClaudeProcess {
    fn spawn(command: &str, extra_args: &[String]) -> std::io::Result<Self> {
        let mut cmd = Command::new(command);
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

    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn write_prompt(&mut self, prompt: &str) -> Result<(), String> {
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
    pub async fn next_message(&mut self, buf: &mut String) -> Result<StreamMessage, String> {
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
            if let Ok(msg) = serde_json::from_str::<StreamMessage>(buf)
                && !matches!(msg, StreamMessage::Other)
            {
                return Ok(msg);
            }
        }
    }

    /// Buffered: collect entire response with usage info
    pub async fn recv(&mut self) -> Result<RecvResult, String> {
        let pid = self.pid();
        let start = Instant::now();
        let mut text = String::with_capacity(4096);
        let mut buf = String::with_capacity(512);
        let mut first_token = true;
        let mut usage = Usage::default();

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
                        return Ok(RecvResult { text, usage });
                    }
                    _ => {}
                },
                StreamMessage::Result {
                    result,
                    is_error,
                    usage: result_usage,
                } => {
                    if is_error {
                        return Err(result);
                    }
                    if let Some(u) = result_usage {
                        usage = u;
                    }
                    let text = if text.is_empty() { result } else { text };
                    return Ok(RecvResult { text, usage });
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
    command: Arc<String>,
}

impl ClaudePool {
    pub fn new(models: &[(&str, usize)]) -> Self {
        Self::new_with_command(models, "claude".into())
    }

    pub fn new_with_command(models: &[(&str, usize)], command: String) -> Self {
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

        let command = Arc::new(command);
        let pool = ClaudePool {
            pools: Arc::new(pools),
            next_req_id: Arc::new(AtomicU64::new(1)),
            command: command.clone(),
        };

        // Spawn a replenisher task per model
        for (model, _) in pool.pools.iter() {
            let pools = Arc::clone(&pool.pools);
            let model = model.clone();
            let cmd = command.clone();
            tokio::spawn(async move {
                let mp = pools.get(&model).unwrap();
                fill_pool(&cmd, &mp.warm, &mp.args, mp.target).await;
                let label = if model.is_empty() { "default" } else { &model };
                tracing::info!("claude pool ready: {label} ({} processes)", mp.target);

                loop {
                    mp.replenish.notified().await;
                    fill_pool(&cmd, &mp.warm, &mp.args, mp.target).await;
                }
            });
        }

        pool
    }

    pub fn next_req_id(&self) -> u64 {
        self.next_req_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Take a process from the pool for the given model, or spawn on-demand.
    /// `extra_args` are CLI flags beyond `--model` (e.g. `--system-prompt`).
    pub async fn take(
        &self,
        model: Option<&str>,
        extra_args: &[String],
    ) -> Result<(ClaudeProcess, &'static str), String> {
        if extra_args.is_empty() {
            let key = model.unwrap_or("");
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
                let proc =
                    ClaudeProcess::spawn(&self.command, &mp.args).map_err(|e| e.to_string())?;
                return Ok((proc, "on-demand"));
            }
        }

        // No pool for this model or has extra args: spawn dedicated process
        let mut args = extra_args.to_vec();
        if let Some(m) = model {
            args.extend(["--model".into(), m.into()]);
        }
        let proc = ClaudeProcess::spawn(&self.command, &args).map_err(|e| e.to_string())?;
        Ok((proc, "custom-args"))
    }
}

async fn fill_pool(
    command: &str,
    warm: &Mutex<Vec<ClaudeProcess>>,
    args: &[String],
    target: usize,
) {
    let current = warm.lock().await.len();
    let needed = target.saturating_sub(current);
    if needed == 0 {
        return;
    }

    let handles: Vec<_> = (0..needed)
        .map(|_| {
            let args = args.to_vec();
            let cmd = command.to_string();
            tokio::spawn(async move { ClaudeProcess::spawn(&cmd, &args) })
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
