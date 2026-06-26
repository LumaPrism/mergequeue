//! App-level client. Holds the RS256 key and produces installation-scoped
//! clients; octocrab handles the JWT → installation-token exchange and refresh.
//!
//! Credentials are resolved from config (a static escape hatch) or from
//! the DB row created by the manifest setup flow — see `crate::setup`.

use jsonwebtoken::EncodingKey;
use octocrab::Octocrab;
use octocrab::models::{AppId, InstallationId};
use secrecy::ExposeSecret;

use super::GitHubError;
use crate::config::GitHubConfig;

/// The GitHub App's credentials, however they were obtained.
#[derive(Clone)]
pub struct AppCredentials {
    pub app_id: u64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub private_key: String,
    pub webhook_secret: String,
}

impl AppCredentials {
    /// From static config (`MQ_GITHUB__*`). Returns the PEM resolved from path or inline.
    pub fn from_config(cfg: &GitHubConfig) -> std::io::Result<Self> {
        Ok(Self {
            app_id: cfg.app_id,
            slug: cfg.app_slug.clone(),
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.expose_secret().to_owned(),
            private_key: cfg.private_key_pem()?,
            webhook_secret: cfg.webhook_secret.expose_secret().to_owned(),
        })
    }
}

#[derive(Clone)]
pub struct AppClient {
    app: Octocrab,
    creds: AppCredentials,
}

impl AppClient {
    pub fn from_credentials(creds: AppCredentials) -> Result<Self, GitHubError> {
        let key = EncodingKey::from_rsa_pem(creds.private_key.as_bytes())?;
        let app = Octocrab::builder().app(AppId(creds.app_id), key).build()?;
        Ok(Self { app, creds })
    }

    /// A client scoped to a single installation. Tokens are minted and cached
    /// by octocrab; safe to call per request.
    pub fn installation(&self, installation_id: u64) -> Result<Octocrab, GitHubError> {
        Ok(self.app.installation(InstallationId(installation_id))?)
    }

    /// The app-level client (e.g. to list installations).
    pub fn app(&self) -> &Octocrab {
        &self.app
    }

    pub fn webhook_secret(&self) -> &str {
        &self.creds.webhook_secret
    }

    pub fn slug(&self) -> &str {
        &self.creds.slug
    }
}
