//! Configuration: YAML file + `MQ_`-prefixed env overrides (`__` nests).
//!
//! Secrets are `secrecy::SecretString`, never logged; the GitHub
//! App private key is loaded from a path in production, inline only for dev.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Config {
    /// Static App credentials (static escape hatch). When absent, the App
    /// is created/loaded via the manifest setup flow (DB-backed).
    pub github: Option<GitHubConfig>,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    /// Master key for at-rest encryption of the DB-stored App secrets. Absent
    /// pre-setup; required once a `github_app` row holds encrypted values.
    #[serde(default)]
    pub secret: Option<SecretConfig>,
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

#[derive(Clone, Deserialize)]
pub struct SecretConfig {
    /// 32-character key, used directly as the 32 key bytes. Wins over `key_file`.
    pub key: Option<SecretString>,
    /// Path to a file holding the 32-character key (trailing newline trimmed).
    pub key_file: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigKeyError {
    #[error("no encryption key configured (set MQ_SECRET__KEY or MQ_SECRET__KEY_FILE)")]
    Missing,
    #[error("could not read MQ_SECRET__KEY_FILE: {0}")]
    KeyFile(#[from] std::io::Error),
    #[error("encryption key must be exactly 32 bytes, got {0}")]
    BadLength(usize),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}

impl SecretConfig {
    /// Resolve the raw 32-byte key: inline `key` wins, else `key_file` contents.
    pub fn resolve(&self) -> Result<secrecy::zeroize::Zeroizing<Vec<u8>>, ConfigKeyError> {
        let raw = if let Some(k) = &self.key {
            k.expose_secret().to_owned()
        } else if let Some(path) = &self.key_file {
            std::fs::read_to_string(path)?.trim_end().to_owned()
        } else {
            return Err(ConfigKeyError::Missing);
        };
        let bytes = raw.into_bytes();
        if bytes.len() != 32 {
            return Err(ConfigKeyError::BadLength(bytes.len()));
        }
        Ok(secrecy::zeroize::Zeroizing::new(bytes))
    }
}

/// Build the crypto handle from config. `None` when no key is configured.
pub fn crypto_from_config(
    cfg: &Config,
) -> Result<Option<crate::crypto::SecretCrypto>, ConfigKeyError> {
    match &cfg.secret {
        Some(s) => {
            let key = s.resolve()?;
            Ok(Some(crate::crypto::SecretCrypto::new(&key[..])?))
        }
        None => Ok(None),
    }
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

#[cfg(test)]
mod secret_tests {
    use super::*;
    use secrecy::SecretString;

    fn cfg(key: &str) -> SecretConfig {
        SecretConfig {
            key: Some(SecretString::from(key.to_string())),
            key_file: None,
        }
    }

    #[test]
    fn accepts_32_byte_key() {
        let bytes = cfg("SaorcGejM8KgmKFsYjxKh22K5DhE2YO1").resolve().unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn rejects_short_key() {
        assert!(cfg("too-short").resolve().is_err());
    }

    #[test]
    fn rejects_long_key() {
        assert!(
            cfg("SaorcGejM8KgmKFsYjxKh22K5DhE2YO1-EXTRA")
                .resolve()
                .is_err()
        );
    }
}
