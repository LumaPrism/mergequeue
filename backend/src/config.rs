//! Configuration: YAML file + `MQ_`-prefixed env overrides (`__` nests).
//!
//! Secrets are `secrecy::SecretString`, never logged; the GitHub
//! App private key is loaded from a path in production, inline only for dev.

use std::fmt;

use secrecy::SecretString;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Config {
    /// Static App credentials (static escape hatch). When absent, the App
    /// is created/loaded via the manifest setup flow (DB-backed).
    pub github: Option<GitHubConfig>,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
}

#[derive(Clone, Deserialize)]
pub struct GitHubConfig {
    pub app_id: u64,
    pub app_slug: String,
    pub client_id: String,
    pub client_secret: SecretString,
    /// Path to the PEM private key. Preferred in production.
    pub private_key_path: Option<String>,
    /// Inline PEM. Dev convenience; `private_key_path` wins if both are set.
    pub private_key: Option<SecretString>,
    pub webhook_secret: SecretString,
}

#[derive(Clone, Deserialize)]
pub struct ServerConfig {
    /// Browser-reachable base, e.g. http://localhost:8080 (manifest redirect target).
    pub base_url: String,
    /// The full, GitHub-reachable webhook endpoint, used verbatim. In local dev
    /// this is your smee.io channel (which forwards to `{base_url}/webhooks/github`);
    /// behind a tunnel it's e.g. `https://x.ngrok.io/webhooks/github`. When unset,
    /// the endpoint defaults to `{base_url}/webhooks/github`.
    pub public_url: Option<String>,
    /// Browser-reachable URL of the dashboard UI (post-login redirect target). In
    /// dev the Next app runs separately; defaults to `base_url` (prod serves both).
    pub app_url: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl ServerConfig {
    /// The URL GitHub posts webhooks to. `public_url` (e.g. an smee.io channel) is
    /// taken verbatim; otherwise the local `/webhooks/github` route under `base_url`.
    pub fn webhook_url(&self) -> String {
        match &self.public_url {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => format!("{}/webhooks/github", self.base_url.trim_end_matches('/')),
        }
    }

    /// Browser-reachable dashboard URL (post-login redirect). Defaults to `base_url`.
    pub fn app_url(&self) -> &str {
        self.app_url.as_deref().unwrap_or(&self.base_url)
    }
}

#[derive(Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: SecretString,
}

fn default_port() -> u16 {
    8080
}

impl Config {
    /// Load `config/<RUN_MODE|default>.yaml` then apply `MQ_*` env overrides.
    pub fn load() -> Result<Self, config::ConfigError> {
        let mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());
        config::Config::builder()
            .add_source(config::File::with_name(&format!("config/{mode}")).required(false))
            .add_source(
                config::Environment::with_prefix("MQ")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?
            .try_deserialize()
    }
}

impl GitHubConfig {
    /// The resolved PEM bytes, from path if present, else inline.
    pub fn private_key_pem(&self) -> std::io::Result<String> {
        use secrecy::ExposeSecret;
        if let Some(path) = &self.private_key_path {
            std::fs::read_to_string(path)
        } else if let Some(pem) = &self.private_key {
            Ok(pem.expose_secret().to_owned())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no GitHub App private key configured (set private_key_path or private_key)",
            ))
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("github", &self.github)
            .field("server", &self.server)
            .field("database", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for GitHubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubConfig")
            .field("app_id", &self.app_id)
            .field("app_slug", &self.app_slug)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("private_key", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("base_url", &self.base_url)
            .field("public_url", &self.public_url)
            .field("app_url", &self.app_url)
            .field("port", &self.port)
            .finish()
    }
}
