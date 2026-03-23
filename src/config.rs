use clap::{Args, Parser, Subcommand};
use rand::Rng;
use serde::{Deserialize, Serialize};

const DEFAULT_PORT: u16 = 19280;
const DEFAULT_CORS_ORIGIN: &str = "";
const DEFAULT_CLAUDE_POOL_SIZE: usize = 2;
const DEFAULT_CONFIG: &str = "~/.config/api-proxy.toml";

#[derive(Args, Deserialize, Serialize, Default, Clone)]
pub struct Settings {
    /// Port to listen on
    #[arg(long)]
    pub port: Option<u16>,
    /// Allowed CORS origin ("*" for any)
    #[arg(long)]
    pub cors_origin: Option<String>,
    /// Number of pre-warmed Claude CLI processes per model
    #[arg(long)]
    pub claude_pool_size: Option<usize>,
    /// Regex pattern for origins to block (empty = no blocking)
    #[arg(long)]
    pub blocked_origin_pattern: Option<String>,
    /// Auth token (auto-generated if missing, read from config file only)
    #[serde(default)]
    #[arg(skip)]
    pub token: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print the current auth token
    GetToken,
    /// Regenerate the auth token and save to config
    RegenerateToken,
    /// Show current configuration
    ShowConfig,
    /// Set the allowed CORS origin ("*" for any, "" for localhost+github.io only)
    SetCorsOrigin {
        /// Origin value, e.g. "https://example.com" or "*"
        origin: String,
    },
    /// Set a regex pattern for origins to block (empty string to disable blocking)
    SetBlockedOrigin {
        /// Regex pattern, e.g. "^chrome-extension://" or "" to disable
        pattern: String,
    },
}

#[derive(Parser)]
#[command(about = "Local proxy for GitHub API and Claude CLI")]
pub struct Config {
    #[command(flatten)]
    pub settings: Settings,
    /// Path to the TOML config file
    #[arg(long, default_value = DEFAULT_CONFIG)]
    pub config: String,
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Config {
    pub fn load() -> Self {
        let mut config = Config::parse();
        let path = shellexpand::tilde(&config.config).to_string();
        let file: Settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();

        if config.settings.port.is_none() {
            config.settings.port = file.port;
        }
        if config.settings.cors_origin.is_none() {
            config.settings.cors_origin = file.cors_origin;
        }
        if config.settings.claude_pool_size.is_none() {
            config.settings.claude_pool_size = file.claude_pool_size;
        }
        if config.settings.blocked_origin_pattern.is_none() {
            config.settings.blocked_origin_pattern = file.blocked_origin_pattern;
        }
        if config.settings.token.is_none() {
            config.settings.token = file.token;
        }

        if config.settings.token.is_none() {
            let token = generate_token();
            config.settings.token = Some(token);
            save_config(&path, &config.settings);
        }

        config
    }

    pub fn config_path(&self) -> String {
        shellexpand::tilde(&self.config).to_string()
    }

    pub fn regenerate_token(&mut self) {
        let token = generate_token();
        self.settings.token = Some(token);
        save_config(&self.config_path(), &self.settings);
    }

    pub fn set_cors_origin(&mut self, origin: String) {
        self.settings.cors_origin = Some(origin);
        save_config(&self.config_path(), &self.settings);
    }

    pub fn set_blocked_origin_pattern(&mut self, pattern: String) {
        self.settings.blocked_origin_pattern = Some(pattern);
        save_config(&self.config_path(), &self.settings);
    }

    pub fn show(&self) {
        println!("Config file:              {}", self.config_path());
        println!("Port:                     {}", self.port());
        let cors = match self.cors_origin() {
            "*" => "any (*)".to_string(),
            "" => "localhost + *.github.io (default)".to_string(),
            v => v.to_string(),
        };
        println!("CORS origin:              {cors}");
        println!(
            "Blocked origin pattern:   {}",
            self.blocked_origin_pattern().unwrap_or("(none)")
        );
    }

    pub fn port(&self) -> u16 {
        self.settings.port.unwrap_or(DEFAULT_PORT)
    }

    pub fn cors_origin(&self) -> &str {
        self.settings
            .cors_origin
            .as_deref()
            .unwrap_or(DEFAULT_CORS_ORIGIN)
    }

    pub fn claude_pool_size(&self) -> usize {
        self.settings
            .claude_pool_size
            .unwrap_or(DEFAULT_CLAUDE_POOL_SIZE)
    }

    pub fn blocked_origin_pattern(&self) -> Option<&str> {
        match self.settings.blocked_origin_pattern.as_deref() {
            // not set → use default extension pattern
            None => Some("^(chrome-extension|moz-extension|safari-web-extension|extension)://"),
            // explicitly set to empty → no blocking
            Some("") => None,
            Some(p) => Some(p),
        }
    }

    pub fn token(&self) -> &str {
        self.settings.token.as_deref().unwrap()
    }
}

fn generate_token() -> String {
    rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

fn save_config(path: &str, settings: &Settings) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let content = toml::to_string_pretty(settings).unwrap();

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
        {
            let _ = f.write_all(content.as_bytes());
        }
    }

    #[cfg(not(unix))]
    {
        let _ = std::fs::write(path, &content);
    }
}
