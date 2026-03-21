# api-proxy

*A local HTTP server that exposes the GitHub CLI and Claude CLI as REST endpoints, so browser apps and scripts can use them without direct CLI access.*

## Features

- **GitHub API passthrough** -- Proxies any GitHub API call through your authenticated `gh` CLI, preserving tokens and permissions
- **Claude CLI gateway** -- Pre-warmed process pools with buffered and SSE streaming endpoints
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

### Claude (buffered)

```bash
TOKEN=$(api-proxy get-token)
curl -s -X POST http://localhost:19280/claude \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Say hello in 3 words", "model": "haiku"}'
# {"response": "Hello, World!"}
```

### Claude (SSE streaming)

```bash
curl -N -X POST http://localhost:19280/claude/stream \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Count to 5", "model": "sonnet"}'
```

#### Request fields

| Field | Type | Description |
|---|---|---|
| `prompt` | string | Required. The prompt text |
| `model` | string | Model alias: `haiku`, `sonnet`, `opus` |
| `effort` | string | Effort level: `low`, `medium`, `high`, `max` |
| `fallback_model` | string | Auto-fallback when primary model is overloaded |
| `system_prompt` | string | Custom system prompt for this request |

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

// Claude (buffered)
const res = await fetch(`${API}/claude`, {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${token}`,
  },
  body: JSON.stringify({ prompt: 'Hello', model: 'haiku' }),
});
const { response } = await res.json();
```

### 3. Stream Claude responses

```js
const res = await fetch(`${API}/claude/stream`, {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${token}`,
  },
  body: JSON.stringify({ prompt: 'Count to 10', model: 'sonnet' }),
});

const reader = res.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;

  const chunk = decoder.decode(value);
  for (const line of chunk.split('\n')) {
    if (!line.startsWith('data: ')) continue;
    const data = line.slice(6);
    if (data === '[DONE]') return;
    process.stdout.write(data); // or append to DOM
  }
}
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
