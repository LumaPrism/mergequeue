//! GitHub App manifest setup flow. `GET /setup` renders an auto-submitting form
//! that POSTs an App manifest to GitHub; GitHub creates the App and redirects to
//! `GET /setup/callback?code=…`, which exchanges the code for the App's
//! credentials and persists them (single `github_app` row). The App client is
//! then built from `resolve_credentials` (config wins, else this DB row).

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use poem::http::StatusCode;
use poem::web::{Data, Html, Query, Redirect};
use poem::{Error, IntoResponse, Response, Result, handler};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;
use crate::error::Result as AppResult;
use crate::github::AppCredentials;
use crate::runtime::Runtime;

/// Short-lived CSRF state tokens for the manifest round-trip (in-process).
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Resolve the App credentials: static config (escape hatch) wins, else the
/// manifest-created DB row. `None` means the App isn't set up yet.
pub async fn resolve_credentials(
    cfg: &Config,
    db: &DatabaseConnection,
) -> AppResult<Option<AppCredentials>> {
    if let Some(gh) = &cfg.github {
        return Ok(Some(AppCredentials::from_config(gh)?));
    }
    load_from_db(db).await
}

async fn load_from_db(db: &DatabaseConnection) -> AppResult<Option<AppCredentials>> {
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
        client_secret: r.try_get("", "client_secret")?,
        private_key: r.try_get("", "private_key")?,
        webhook_secret: r.try_get("", "webhook_secret")?,
    }))
}

async fn save_to_db(db: &DatabaseConnection, c: &AppCredentials, html_url: &str) -> AppResult<()> {
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
            Value::from(c.client_secret.clone()),
            Value::from(c.private_key.clone()),
            Value::from(c.webhook_secret.clone()),
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

#[derive(Deserialize)]
pub struct StartQuery {
    /// Create the App under this org (e.g. `?org=withoneai`) so a *private* App can
    /// be installed there. Omit to create it under your personal account.
    org: Option<String>,
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

/// `GET /setup` — render an auto-submitting form that POSTs the manifest to GitHub.
/// Pass `?org=<login>` to create the App under an organization you administer.
#[handler]
pub async fn start(
    cfg: Data<&Config>,
    db: Data<&DatabaseConnection>,
    Query(q): Query<StartQuery>,
) -> Result<Response> {
    if let Some(c) = resolve_credentials(&cfg, &db)
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
    {
        return Ok(Redirect::see_other(format!(
            "https://github.com/apps/{}/installations/new",
            c.slug
        ))
        .into_response());
    }
    if let Some(org) = &q.org
        && !valid_org(org)
    {
        return Err(Error::from_string(
            "invalid org login",
            StatusCode::BAD_REQUEST,
        ));
    }
    let state = Uuid::new_v4().to_string();
    PENDING.lock().unwrap().insert(state.clone());
    let manifest = manifest(&cfg)
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

    save_to_db(&db, &creds, html_url)
        .await
        .map_err(|e| Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;

    // Build the engine + webhook secret now so the instance works without a restart.
    if let Err(e) = rt.reinit().await {
        tracing::error!(error = %e, "post-setup engine init failed");
    }

    // App created — send the operator to install it on their repos.
    Ok(Redirect::see_other(format!(
        "https://github.com/apps/{}/installations/new",
        creds.slug
    )))
}
