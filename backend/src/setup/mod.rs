//! GitHub App manifest setup flow. `GET /setup` renders an auto-submitting form
//! that POSTs an App manifest to GitHub; GitHub creates the App and redirects to
//! `GET /setup/callback?code=…`, which exchanges the code for the App's
//! credentials and persists them (single `github_app` row). The App client is
//! then built from [`SetupService::resolve_credentials`] (config wins, else this
//! DB row).

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex, Once};

use poem::http::StatusCode;
use poem::web::{Data, Html, Query, Redirect};
use poem::{Error, IntoResponse, Response, Result, handler};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;
use crate::crypto::{CryptoError, SecretCrypto};
use crate::error::Result as AppResult;
use crate::github::AppCredentials;
use crate::runtime::Runtime;

const ENC_PREFIX: &str = "enc:";

static PLAINTEXT_WARNED: Once = Once::new();

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("github_app secrets are encrypted but MQ_SECRET__KEY is missing or incorrect")]
    KeyMissing,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Encrypt a secret for storage: `enc:` + hex(nonce‖ct‖tag).
fn seal(crypto: &SecretCrypto, plain: &str) -> Result<String, CryptoError> {
    Ok(format!("{ENC_PREFIX}{}", crypto.encrypt(plain)?))
}

/// Decrypt a stored secret. `enc:`-prefixed values require the key; legacy
/// plaintext (no prefix) passes through with a one-time warning (emitted at
/// most once per process via [`PLAINTEXT_WARNED`]).
fn open(crypto: Option<&SecretCrypto>, stored: &str) -> Result<String, OpenError> {
    match stored.strip_prefix(ENC_PREFIX) {
        Some(hexed) => Ok(crypto.ok_or(OpenError::KeyMissing)?.decrypt(hexed)?),
        None => {
            PLAINTEXT_WARNED.call_once(|| {
                tracing::warn!(
                    "github_app secret stored unencrypted; set MQ_SECRET__KEY to encrypt at rest"
                );
            });
            Ok(stored.to_owned())
        }
    }
}

/// Short-lived CSRF state tokens for the manifest round-trip (in-process).
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// GitHub App manifest setup. Zero-sized; all behavior is associated functions.
/// The `#[handler]` route fns ([`start`], [`callback`]) stay at module root (poem
/// requires it) and delegate here.
pub struct SetupService;

impl SetupService {
    /// Resolve the App credentials: static config (escape hatch) wins, else the
    /// manifest-created DB row. `None` means the App isn't set up yet.
    pub async fn resolve_credentials(
        cfg: &Config,
        db: &DatabaseConnection,
    ) -> AppResult<Option<AppCredentials>> {
        if let Some(gh) = &cfg.github {
            return Ok(Some(AppCredentials::from_config(gh)?));
        }
        let crypto = crate::config::crypto_from_config(cfg)?;
        Self::load_from_db(db, crypto.as_ref()).await
    }

    async fn load_from_db(
        db: &DatabaseConnection,
        crypto: Option<&SecretCrypto>,
    ) -> AppResult<Option<AppCredentials>> {
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT app_id, slug, client_id, client_secret, private_key, webhook_secret \
                 FROM github_app WHERE id = 1",
            ))
            .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(AppCredentials {
            app_id: r.try_get::<i64>("", "app_id")? as u64,
            slug: r.try_get("", "slug")?,
            client_id: r.try_get("", "client_id")?,
            client_secret: open(crypto, &r.try_get::<String>("", "client_secret")?)?,
            private_key: open(crypto, &r.try_get::<String>("", "private_key")?)?,
            webhook_secret: open(crypto, &r.try_get::<String>("", "webhook_secret")?)?,
        }))
    }

    async fn save_to_db(
        db: &DatabaseConnection,
        crypto: &SecretCrypto,
        c: &AppCredentials,
        html_url: &str,
    ) -> AppResult<()> {
        let client_secret = seal(crypto, &c.client_secret)?;
        let private_key = seal(crypto, &c.private_key)?;
        let webhook_secret = seal(crypto, &c.webhook_secret)?;
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO github_app \
               (id, app_id, slug, client_id, client_secret, private_key, webhook_secret, html_url) \
             VALUES (1, $1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (id) DO UPDATE SET \
               app_id = $1, slug = $2, client_id = $3, client_secret = $4, \
               private_key = $5, webhook_secret = $6, html_url = $7",
            [
                Value::from(c.app_id as i64),
                Value::from(c.slug.clone()),
                Value::from(c.client_id.clone()),
                Value::from(client_secret),
                Value::from(private_key),
                Value::from(webhook_secret),
                Value::from(html_url.to_owned()),
            ],
        ))
        .await?;
        Ok(())
    }

    /// The GitHub App manifest — permissions/events match what the engine needs.
    fn manifest(cfg: &Config) -> serde_json::Value {
        let base = cfg.server.base_url.trim_end_matches('/');
        serde_json::json!({
            "name": "mergequeue",
            "url": base,
            "hook_attributes": { "url": cfg.server.webhook_url(), "active": true },
            "redirect_url": format!("{base}/setup/callback"),
            "callback_urls": [format!("{}/auth/github/callback", cfg.server.app_url().trim_end_matches('/'))],
            "request_oauth_on_install": false,
            "public": false,
            "default_permissions": {
                "contents": "write",
                "pull_requests": "write",
                "issues": "write",
                "administration": "read",
                "checks": "read",
                "statuses": "read",
                "metadata": "read"
            },
            "default_events": [
                "pull_request", "check_run", "status", "push", "issue_comment"
            ]
        })
    }

    /// If the stored row still holds plaintext secrets and a key is configured,
    /// rewrite the three secret columns in `enc:` form (a one-time upgrade of a
    /// pre-encryption row). No-op when no key is set or the row is already
    /// encrypted. Cheap (one row), called from `reinit`.
    pub async fn reencrypt_legacy_if_needed(
        db: &DatabaseConnection,
        crypto: Option<&SecretCrypto>,
        creds: &AppCredentials,
    ) -> AppResult<()> {
        let Some(crypto) = crypto else { return Ok(()) };
        let Some(row) = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT client_secret, private_key, webhook_secret FROM github_app WHERE id = 1",
            ))
            .await?
        else {
            return Ok(());
        };
        let is_plain = |col: &str| -> bool {
            row.try_get::<String>("", col)
                .map(|v| !v.starts_with(ENC_PREFIX))
                .unwrap_or(false)
        };
        if !(is_plain("client_secret") || is_plain("private_key") || is_plain("webhook_secret")) {
            return Ok(());
        }
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE github_app SET client_secret = $1, private_key = $2, webhook_secret = $3 WHERE id = 1",
            [
                Value::from(seal(crypto, &creds.client_secret)?),
                Value::from(seal(crypto, &creds.private_key)?),
                Value::from(seal(crypto, &creds.webhook_secret)?),
            ],
        ))
        .await?;
        tracing::info!("github_app secrets re-encrypted at rest");
        Ok(())
    }

    /// A GitHub org/user login: 1–39 chars of `[A-Za-z0-9-]`, no leading/trailing
    /// hyphen. Validated before interpolation so `?org=` can't break out of the
    /// form's `action` attribute or inject into the redirect URL.
    fn valid_org(org: &str) -> bool {
        !org.is_empty()
            && org.len() <= 39
            && !org.starts_with('-')
            && !org.ends_with('-')
            && org.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
}

#[derive(Deserialize)]
pub struct StartQuery {
    /// Create the App under this org (e.g. `?org=withoneai`) so a *private* App can
    /// be installed there. Omit to create it under your personal account.
    org: Option<String>,
}

/// `GET /setup` — render an auto-submitting form that POSTs the manifest to GitHub.
/// Pass `?org=<login>` to create the App under an organization you administer.
#[handler]
pub async fn start(
    cfg: Data<&Config>,
    db: Data<&DatabaseConnection>,
    Query(q): Query<StartQuery>,
) -> Result<Response> {
    if let Some(c) = SetupService::resolve_credentials(&cfg, &db)
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
    {
        return Ok(Redirect::see_other(format!(
            "https://github.com/apps/{}/installations/new",
            c.slug
        ))
        .into_response());
    }
    // Fail closed BEFORE sending the operator to GitHub: registering consumes a
    // single-use manifest code, so we must know we can encrypt the returned
    // secrets first. Otherwise a missing key burns the App registration.
    if crate::config::crypto_from_config(&cfg)
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
        .is_none()
    {
        return Err(Error::from_string(
            "MQ_SECRET__KEY is not set. Generate one with `just gen-secret-key` \
             (or `openssl rand -base64 24`) and set it before registering the App.",
            StatusCode::BAD_REQUEST,
        ));
    }

    if let Some(org) = &q.org
        && !SetupService::valid_org(org)
    {
        return Err(Error::from_string(
            "invalid org login",
            StatusCode::BAD_REQUEST,
        ));
    }
    let state = Uuid::new_v4().to_string();
    PENDING.lock().unwrap().insert(state.clone());
    let manifest = SetupService::manifest(&cfg)
        .to_string()
        .replace('&', "&amp;")
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let action = match &q.org {
        Some(org) => {
            format!("https://github.com/organizations/{org}/settings/apps/new?state={state}")
        }
        None => format!("https://github.com/settings/apps/new?state={state}"),
    };
    Ok(Html(format!(
        "<!doctype html><html><head><meta charset=utf-8><title>create mergequeue app</title></head>\
         <body style=\"background:#0c0d10;color:#e6e4df;font-family:sans-serif;display:grid;place-items:center;height:100vh\">\
           <form id=f action=\"{action}\" method=post>\
             <input type=hidden name=manifest value='{manifest}'>\
             <noscript><button type=submit>Create the GitHub App</button></noscript>\
           </form>\
           <p>Redirecting to GitHub to create the app…</p>\
           <script>document.getElementById('f').submit()</script>\
         </body></html>"
    ))
    .into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: Option<String>,
}

/// `GET /setup/callback?code=…&state=…` — exchange the code for the App's
/// credentials, persist them, and send the operator on to install the App.
#[handler]
pub async fn callback(
    Query(q): Query<CallbackQuery>,
    cfg: Data<&Config>,
    db: Data<&DatabaseConnection>,
    rt: Data<&Arc<Runtime>>,
) -> Result<Redirect> {
    let state = q
        .state
        .as_deref()
        .ok_or_else(|| Error::from_string("missing setup state", StatusCode::BAD_REQUEST))?;
    if !PENDING.lock().unwrap().remove(state) {
        return Err(Error::from_string(
            "unknown setup state",
            StatusCode::BAD_REQUEST,
        ));
    }

    let v: serde_json::Value = reqwest::Client::new()
        .post(format!(
            "https://api.github.com/app-manifests/{}/conversions",
            q.code
        ))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "mergequeue")
        .send()
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::BAD_GATEWAY))?
        .json()
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::BAD_GATEWAY))?;

    let pick = |k: &str| -> Result<String> {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::from_string(
                    format!("manifest response missing {k}"),
                    StatusCode::BAD_GATEWAY,
                )
            })
    };
    let creds = AppCredentials {
        app_id: v
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                Error::from_string("manifest response missing id", StatusCode::BAD_GATEWAY)
            })?,
        slug: pick("slug")?,
        client_id: pick("client_id")?,
        client_secret: pick("client_secret")?,
        private_key: pick("pem")?,
        webhook_secret: pick("webhook_secret")?,
    };
    let html_url = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .unwrap_or_default();

    let crypto = crate::config::crypto_from_config(&cfg)
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
        .ok_or_else(|| {
            Error::from_string(
                "set MQ_SECRET__KEY before registering the App (generate one with `just gen-secret-key` or `openssl rand -base64 24`)",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    SetupService::save_to_db(&db, &crypto, &creds, html_url)
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;

    if let Err(e) = rt.reinit().await {
        tracing::error!(error = %e, "post-setup engine init failed");
    }

    Ok(Redirect::see_other(format!(
        "https://github.com/apps/{}/installations/new",
        creds.slug
    )))
}

#[cfg(test)]
mod reencrypt_tests {
    use super::*;
    use sea_orm::Database;

    // Defaults to the THROWAWAY test DB, never the live `mergequeue` DB, so this
    // test can seed/overwrite github_app id=1 freely.
    async fn db() -> DatabaseConnection {
        let url = std::env::var("MQ_TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5433/mergequeue_test".into()
        });
        Database::connect(url).await.unwrap()
    }

    #[tokio::test]
    async fn legacy_plaintext_row_is_reencrypted() {
        let db = db().await;
        let crypto = SecretCrypto::new(b"SaorcGejM8KgmKFsYjxKh22K5DhE2YO1").unwrap();
        // Seed a plaintext row (pre-encryption shape).
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "INSERT INTO github_app (id,app_id,slug,client_id,client_secret,private_key,webhook_secret,html_url) \
             VALUES (1,1,'s','cid','plain-cs','plain-pk','plain-ws','u') \
             ON CONFLICT (id) DO UPDATE SET client_secret='plain-cs', private_key='plain-pk', webhook_secret='plain-ws'",
        )).await.unwrap();

        let creds = SetupService::load_from_db(&db, Some(&crypto))
            .await
            .unwrap()
            .unwrap();
        SetupService::reencrypt_legacy_if_needed(&db, Some(&crypto), &creds)
            .await
            .unwrap();

        let raw: String = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT private_key FROM github_app WHERE id=1",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "private_key")
            .unwrap();
        assert!(
            raw.starts_with("enc:"),
            "row should be encrypted, got {raw}"
        );
        assert_eq!(open(Some(&crypto), &raw).unwrap(), "plain-pk");
    }
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    fn crypto() -> SecretCrypto {
        SecretCrypto::new(b"SaorcGejM8KgmKFsYjxKh22K5DhE2YO1").unwrap()
    }

    #[test]
    fn seal_then_open_roundtrips() {
        let c = crypto();
        let sealed = seal(&c, "-----BEGIN PRIVATE KEY-----x").unwrap();
        assert!(sealed.starts_with("enc:"));
        assert_eq!(
            open(Some(&c), &sealed).unwrap(),
            "-----BEGIN PRIVATE KEY-----x"
        );
    }

    #[test]
    fn open_passes_legacy_plaintext_through() {
        assert_eq!(
            open(None, "raw-plaintext-secret").unwrap(),
            "raw-plaintext-secret"
        );
    }

    #[test]
    fn open_encrypted_without_key_errors() {
        let sealed = seal(&crypto(), "secret").unwrap();
        assert!(matches!(open(None, &sealed), Err(OpenError::KeyMissing)));
    }
}
