# api-proxy

Local HTTP proxy exposing `gh` and `claude` CLIs as REST endpoints.

## Architecture

- `src/main.rs` -- Axum server setup, routing, CORS, AppState
- `src/config.rs` -- Config from TOML file + CLI args (clap), token generation
- `src/auth.rs` -- Bearer token auth middleware
- `src/claude.rs` -- Claude CLI per-model process pools. Pre-spawns processes using `--input-format stream-json`. Each process serves one request then is dropped (no context reset in stream-json protocol).
- `src/messages.rs` -- Anthropic Messages API compatible endpoint (`/claude/v1/messages`). Handles both buffered and streaming responses.
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
Protected routes: `POST /claude/v1/messages`, `/gh/*`

### For CLI/scripts

```bash
TOKEN=$(api-proxy get-token)
curl -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user
```

## Claude Messages API

`POST /claude/v1/messages` implements the [Anthropic Messages API](https://docs.anthropic.com/en/api/messages) format, allowing SDK clients to use this proxy as a drop-in backend.

### SDK usage

Python:
```python
from anthropic import Anthropic
client = Anthropic(base_url="http://localhost:19280/claude", api_key=TOKEN)
msg = client.messages.create(model="sonnet", max_tokens=1024, messages=[{"role": "user", "content": "Hello"}])
```

TypeScript:
```typescript
import Anthropic from '@anthropic-ai/sdk';
const client = new Anthropic({ baseURL: 'http://localhost:19280/claude', apiKey: TOKEN });
const msg = await client.messages.create({ model: 'sonnet', max_tokens: 1024, messages: [{ role: 'user', content: 'Hello' }] });
```

### curl examples

```bash
TOKEN=$(api-proxy get-token)

# Buffered
curl -s -X POST http://localhost:19280/claude/v1/messages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "sonnet", "max_tokens": 1024, "messages": [{"role": "user", "content": "Say hello"}]}'

# Streaming
curl -N -X POST http://localhost:19280/claude/v1/messages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "sonnet", "max_tokens": 1024, "stream": true, "messages": [{"role": "user", "content": "Count to 5"}]}'
```

### Request fields

| Field | Type | Status | Notes |
|---|---|---|---|
| `model` | string | **Required** | `"haiku"`, `"sonnet"`, `"opus"`, or full Anthropic model IDs |
| `max_tokens` | number | **Required** | Accepted but not enforced (CLI has no equivalent) |
| `messages` | array | **Required** | `[{role, content}]`. Multi-turn is flattened to a single prompt |
| `system` | string | Optional | Passed to CLI via `--system-prompt` |
| `stream` | boolean | Optional | `true` for SSE streaming, `false` (default) for buffered |
| `temperature` | number | Ignored | No CLI equivalent |
| `top_p` | number | Ignored | No CLI equivalent |
| `top_k` | number | Ignored | No CLI equivalent |
| `stop_sequences` | array | Ignored | No CLI equivalent |

### Response differences from Anthropic API

| Field | Notes |
|---|---|
| `id` | Generated locally as `msg_<counter>`, not a real Anthropic message ID |
| `usage.input_tokens` | From CLI result message if available, otherwise `0` |
| `usage.output_tokens` | From CLI result message if available, otherwise `0` |
| `stop_reason` | Always `"end_turn"` on success (no `max_tokens` or `tool_use` detection) |
| `stop_sequence` | Always `null` |

### Limitations

- **Single-turn only**: multi-turn `messages` arrays are flattened into one prompt. Assistant messages are wrapped in `<previous_response>` tags for context.
- **No tools**: all requests run with `--tools ""` (read-only, no file access, no shell)
- **No images**: image content blocks are not supported
- **No sampling control**: `temperature`, `top_p`, `top_k`, `max_tokens` are accepted but ignored
- **No stop sequences**: `stop_sequences` is accepted but ignored

## GitHub API

`/gh/*` proxies requests to the GitHub API via the `gh` CLI.

```bash
TOKEN=$(api-proxy get-token)

# REST
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user

# GraphQL (wrap query in JSON)
curl -s -X POST http://localhost:19280/gh/graphql \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "{ viewer { login } }"}'
```

## Key Design Decisions

- Processes are single-use. The stream-json protocol has no context reset, so reusing a process would leak prior conversation into new requests.
- Per-model pools: pre-warms 2 processes each for default, sonnet, haiku, and opus.
- `--verbose` flag is required for `--output-format stream-json` in current CLI versions.
- Bearer token auth prevents unauthorized access from malicious browser extensions or pages.
