//! GitHub OAuth (user-to-server) flow. `initiate` builds GitHub's authorize URL
//! and registers a fresh in-process CSRF state; `complete` consumes that state,
//! exchanges the code for a token, fetches the GitHub user, enforces the
//! installation authorization gate, and upserts the user.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use reqwest::{Client, Url};
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{AuthError, AuthStore};
use crate::config::Config;
use crate::setup::SetupService;

/// Short-lived CSRF state tokens for the OAuth round-trip (in-process).
static STATES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct GitHubUser {
    id: i64,
    login: String,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct UserInstallations {
    total_count: i64,
}

/// The user upserted by a completed OAuth round-trip.
pub struct Authenticated {
    pub user_pk: Uuid,
    pub login: String,
}

/// GitHub OAuth flow. Zero-sized; all behavior is associated functions.
pub struct OAuthService;

impl OAuthService {
    /// Build GitHub's authorize URL and register a fresh CSRF state for the
    /// round-trip.
    pub async fn initiate(cfg: &Config, db: &DatabaseConnection) -> Result<String, AuthError> {
        let creds = SetupService::resolve_credentials(cfg, db)
            .await?
            .ok_or(AuthError::NotConfigured)?;
        let state = Uuid::new_v4().to_string();
        STATES.lock().unwrap().insert(state.clone());
        let redirect_uri = format!(
            "{}/auth/github/callback",
            cfg.server.app_url().trim_end_matches('/')
        );
        let url = Url::parse_with_params(
            "https://github.com/login/oauth/authorize",
            &[
                ("client_id", creds.client_id.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("state", state.as_str()),
                ("scope", "read:user"),
                ("allow_signup", "false"),
            ],
        )
        .map_err(|e| AuthError::UrlParse(e.to_string()))?;
        Ok(url.to_string())
    }

    /// Consume the CSRF state, exchange the code for a token, fetch the GitHub
    /// user, enforce the installation authorization gate, and upsert the user.
    pub async fn complete(
        cfg: &Config,
        db: &DatabaseConnection,
        code: &str,
        state: &str,
    ) -> Result<Authenticated, AuthError> {
        if !STATES.lock().unwrap().remove(state) {
            return Err(AuthError::UnknownState);
        }
        let creds = SetupService::resolve_credentials(cfg, db)
            .await?
            .ok_or(AuthError::NotConfigured)?;
        let redirect_uri = format!(
            "{}/auth/github/callback",
            cfg.server.app_url().trim_end_matches('/')
        );

        let client = Client::new();
        let token: TokenResponse = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("User-Agent", "mergequeue")
            .json(&serde_json::json!({
                "client_id": creds.client_id,
                "client_secret": creds.client_secret,
                "code": code,
                "redirect_uri": redirect_uri,
            }))
            .send()
            .await?
            .json()
            .await?;
        let access = token.access_token.ok_or(AuthError::NoAccessToken)?;

        let gh_user: GitHubUser = client
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {access}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "mergequeue")
            .send()
            .await?
            .json()
            .await?;

        let installs: UserInstallations = client
            .get("https://api.github.com/user/installations")
            .header("Authorization", format!("Bearer {access}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "mergequeue")
            .send()
            .await?
            .json()
            .await?;
        tracing::info!(
            login = %gh_user.login,
            installs = installs.total_count,
            "oauth callback: authorization gate"
        );
        if installs.total_count == 0 {
            tracing::warn!(
                login = %gh_user.login,
                "oauth callback: rejected — no accessible installation (gate 403)"
            );
            return Err(AuthError::NotAuthorized);
        }

        let user_pk = AuthStore::upsert_user(
            db,
            gh_user.id,
            &gh_user.login,
            gh_user.avatar_url.as_deref().unwrap_or(""),
        )
        .await?;
        Ok(Authenticated {
            user_pk,
            login: gh_user.login,
        })
    }
}
