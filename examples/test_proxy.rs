use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;

use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Value, json};
use tokio::process::Command;

fn random_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn read_token() -> String {
    let path = shellexpand::tilde("~/.config/api-proxy.toml").to_string();
    let content = std::fs::read_to_string(&path)
        .expect("config not found; run the proxy once to generate it");
    let config: toml::Table = content.parse().expect("invalid config TOML");
    config["token"]
        .as_str()
        .expect("no token in config")
        .to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = random_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token();

    // Start the proxy (binary is built alongside this example)
    let bin = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("api-proxy");
    let mut proxy = Command::new(&bin)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;

    println!("proxy started on port {port}");

    // Build client with auth header
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    let client = Client::builder().default_headers(headers).build()?;

    // Wait for it to be ready (health is unauthenticated)
    let health_client = Client::new();
    for _ in 0..50 {
        if health_client
            .get(format!("{base}/health"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 1. Call Claude and ask it to return "Hello, World!"
    println!("=== POST /claude ===");
    let claude_resp = client
        .post(format!("{base}/claude"))
        .json(&json!({
            "prompt": "Reply with exactly: Hello, World",
            "model": "haiku"
        }))
        .send()
        .await?;
    let status = claude_resp.status();
    let body = claude_resp.text().await?;
    println!("status: {status}");
    if status.is_success() {
        let json: Value = serde_json::from_str(&body)?;
        println!("response: {}", json["response"]);
    } else {
        println!("error: {body}");
    }

    // 2. Call the GitHub GraphQL API to get the authenticated user's login and bio
    println!("\n=== POST /gh/graphql ===");
    let gh_resp = client
        .post(format!("{base}/gh/graphql"))
        .json(&json!({
            "query": "query { viewer { login name bio } }"
        }))
        .send()
        .await?;
    let status = gh_resp.status();
    let body = gh_resp.text().await?;
    println!("status: {status}");
    if status.is_success() {
        let json: Value = serde_json::from_str(&body)?;
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("error: {body}");
    }

    proxy.kill().await?;
    Ok(())
}
