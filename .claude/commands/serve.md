---
description: Build and start the api-proxy server
---

Build and run the api-proxy server using the /tmux skill:

1. Build: `cargo build`
2. If a previous api-proxy is running in tmux, kill it first
3. Start in tmux: `RUST_LOG=debug cargo run`
4. Wait for "listening on" in the tmux output before reporting success
