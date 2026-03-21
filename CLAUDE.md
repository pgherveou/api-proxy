# api-proxy

Local HTTP proxy exposing `gh` and `claude` CLIs as REST endpoints.

## Architecture

- `src/main.rs` -- Axum server setup, routing, CORS, AppState
- `src/config.rs` -- Config from TOML file + CLI args (clap), token generation
- `src/auth.rs` -- Bearer token auth middleware
- `src/claude.rs` -- Claude CLI per-model pools, buffered handler, and SSE streaming handler. Pre-spawns processes using `--input-format stream-json`. Each process serves one request then is dropped (no context reset in stream-json protocol).
- `src/gh.rs` -- GitHub API passthrough via `gh api`
- `src/pages.rs` -- Static page handler (test UI at `/`)
- `static/index.html` -- Test UI page (embedded via include_str!)

## Build & Test

```bash
# Build (must pass without warnings)
cargo build

# Format
cargo +nightly fmt

# Run with debug logs
RUST_LOG=debug cargo run

# Install as service (Linux systemd / macOS launchd)
./install.sh
```

## Authentication

All API routes require a bearer token. The token is auto-generated on first run and stored in `~/.config/api-proxy.toml` (mode 0600).

Public routes (no auth required): `GET /health`, `GET /`
Protected routes: `POST /claude`, `POST /claude/stream`, `/gh/*`

### For CLI/scripts

```bash
TOKEN=$(api-proxy get-token)
curl -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user
```

## Manual Testing

Open `http://localhost:19280/` for the built-in test UI, or use curl:

```bash
TOKEN=$(api-proxy get-token)

# Health check (no auth)
curl http://localhost:19280/health

# Claude (buffered)
curl -s -X POST http://localhost:19280/claude \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Say hello in 3 words"}'

# Claude (SSE streaming)
curl -N -X POST http://localhost:19280/claude/stream \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Count to 5"}'

# GitHub API
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user
```

## Claude Request Fields

| Field | Type | Description |
|---|---|---|
| `prompt` | string | Required. The prompt text |
| `model` | string | Model alias: "haiku", "sonnet", "opus" |
| `effort` | string | Effort level: "low", "medium", "high", "max" |
| `fallback_model` | string | Auto-fallback when primary model is overloaded |
| `system_prompt` | string | Custom system prompt for this request |

## Key Design Decisions

- Processes are single-use. The stream-json protocol has no context reset, so reusing a process would leak prior conversation into new requests.
- Per-model pools: pre-warms 2 processes each for default, sonnet, haiku, and opus.
- `--verbose` flag is required for `--output-format stream-json` in current CLI versions.
- `/claude/stream` uses SSE to stream tokens to the client as they arrive, minimizing time-to-first-byte.
- Bearer token auth prevents unauthorized access from malicious browser extensions or pages.
