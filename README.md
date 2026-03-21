# api-proxy

*A local HTTP server that exposes the GitHub CLI and Claude CLI as REST endpoints, so browser apps and scripts can use them without direct CLI access.*

## Features

- **GitHub API passthrough** -- Proxies any GitHub API call through your authenticated `gh` CLI, preserving tokens and permissions
- **Claude CLI gateway** -- [Anthropic Messages API](https://platform.claude.com/docs/en/build-with-claude/working-with-messages) compatible endpoint with pre-warmed process pools and SSE streaming. Requests are read-only (no tools, no file access) with fresh context each time.
- **Bearer token auth** -- Auto-generated token, stored in config with 0600 permissions
- **CORS support** -- Configurable origin policy for browser-based clients
- **Zero credentials in config** -- Delegates auth entirely to `gh` and `claude`, no API keys to manage
- **systemd ready** -- Ships with a user service file for always-on operation

## Quick Start

<details>
<summary>Prerequisites</summary>

- Rust 1.85+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- [GitHub CLI](https://cli.github.com/) (`gh`) authenticated (`gh auth login`)
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-code) installed and on PATH

</details>

```bash
# From GitHub
cargo install --git https://github.com/pgherveou/api-proxy

# Or from a local clone
cargo install --path .
```

Then run:

```bash
api-proxy
```

The server starts on `http://127.0.0.1:19280`. A bearer token is auto-generated on first run and stored in `~/.config/api-proxy.toml`.

## Authentication

All API routes except `GET /health` and `GET /` require a bearer token.

```bash
# Retrieve the token
api-proxy get-token

# Regenerate the token
api-proxy regenerate-token
```

Include it in requests:

```bash
TOKEN=$(api-proxy get-token)
curl -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user
```

The built-in test UI at `http://localhost:19280/` lets you paste the token and test endpoints interactively.

## Usage

### Health check

```bash
curl http://localhost:19280/health
# OK
```

### Claude (`POST /claude/v1/messages`)

Implements the [Anthropic Messages API](https://platform.claude.com/docs/en/build-with-claude/working-with-messages) format. Existing Anthropic SDK clients can point at this proxy as a drop-in backend.

```bash
TOKEN=$(api-proxy get-token)

# Buffered
curl -s -X POST http://localhost:19280/claude/v1/messages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "sonnet", "max_tokens": 1024, "messages": [{"role": "user", "content": "Say hello in 3 words"}]}'

# Streaming
curl -N -X POST http://localhost:19280/claude/v1/messages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "sonnet", "max_tokens": 1024, "stream": true, "messages": [{"role": "user", "content": "Count to 5"}]}'
```

#### SDK usage

```python
from anthropic import Anthropic
client = Anthropic(base_url="http://localhost:19280/claude", api_key=TOKEN)
msg = client.messages.create(model="sonnet", max_tokens=1024, messages=[{"role": "user", "content": "Hello"}])
```

#### Request fields

| Field | Type | Status | Notes |
|---|---|---|---|
| `model` | string | **Required** | `"haiku"`, `"sonnet"`, `"opus"`, or full Anthropic model IDs |
| `max_tokens` | number | **Required** | Accepted but not enforced (no CLI equivalent) |
| `messages` | array | **Required** | `[{role, content}]`. Multi-turn is flattened to a single prompt |
| `system` | string | Optional | Passed to CLI via `--system-prompt` |
| `stream` | boolean | Optional | `true` for SSE streaming |
| `temperature` | number | Ignored | No CLI equivalent |
| `top_p` | number | Ignored | No CLI equivalent |
| `top_k` | number | Ignored | No CLI equivalent |
| `stop_sequences` | array | Ignored | No CLI equivalent |

#### Response differences from Anthropic API

| Field | Notes |
|---|---|
| `id` | Generated locally as `msg_<counter>`, not a real Anthropic message ID |
| `usage` | From CLI result message if available, otherwise `0` |
| `stop_reason` | Always `"end_turn"` on success (no `max_tokens` or `tool_use` detection) |
| `stop_sequence` | Always `null` |

#### Limitations

- **Single-turn only**: multi-turn messages are flattened into one prompt
- **No tools**: all requests run read-only with no file access or shell
- **No images**: image content blocks are not supported
- **No sampling control**: `temperature`, `top_p`, `top_k`, `max_tokens` accepted but ignored

### GitHub API

Requests to `/gh/*` are forwarded to `gh api`. Method, query params, headers, and body are preserved.

```bash
# Get authenticated user
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:19280/gh/user

# GraphQL query
curl -s -X POST http://localhost:19280/gh/graphql \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "query { viewer { login name bio } }"}'
```

## Web App Integration

Web apps running in the browser can use api-proxy as a local backend for Claude and GitHub API calls. The user must have api-proxy running on their machine.

### 1. Obtain the token

The user copies their token from the terminal and pastes it into your app:

```bash
# Linux
api-proxy get-token | xclip -sel clip

# macOS
api-proxy get-token | pbcopy
```

Your app should provide a token input field and store it in `localStorage`:

```js
// Store
localStorage.setItem('api-proxy-token', userInput);

// Retrieve
const token = localStorage.getItem('api-proxy-token');
```

### 2. Make requests

```js
const API = 'http://localhost:19280';
const token = localStorage.getItem('api-proxy-token');

// Claude (uses Anthropic Messages API format)
const res = await fetch(`${API}/claude/v1/messages`, {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${token}`,
  },
  body: JSON.stringify({
    model: 'sonnet',
    max_tokens: 1024,
    messages: [{ role: 'user', content: 'Hello' }],
  }),
});
const { content } = await res.json();
const text = content[0].text;
```

### 4. GitHub API

```js
// REST
const user = await fetch(`${API}/gh/user`, {
  headers: { 'Authorization': `Bearer ${token}` },
}).then(r => r.json());

// GraphQL
const result = await fetch(`${API}/gh/graphql`, {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${token}`,
  },
  body: JSON.stringify({
    query: `query { viewer { login name } }`,
  }),
}).then(r => r.json());
```

### CORS

By default, api-proxy allows requests from any origin. To restrict to your app's domain:

```toml
# ~/.config/api-proxy.toml
cors_origin = "https://myapp.com"
```

## Configuration

api-proxy reads from `~/.config/api-proxy.toml` by default. All fields are optional.

```toml
port = 19280
cors_origin = "*"
claude_pool_size = 2
```

| Option | Default | Description |
|--------|---------|-------------|
| `port` | `19280` | Port to listen on |
| `cors_origin` | `*` | Allowed CORS origin (`*` for any, or a specific origin) |
| `claude_pool_size` | `2` | Number of pre-warmed Claude CLI processes per model |

CLI flags override the config file:

```bash
api-proxy --port 8080
api-proxy --config /path/to/config.toml
```

## Running as a systemd service

```bash
./install.sh
```

Or manually:

```bash
cp api-proxy.service ~/.config/systemd/user/
systemctl --user enable --now api-proxy
```

Check status:

```bash
systemctl --user status api-proxy
journalctl --user -u api-proxy -f
```

## License

MIT
