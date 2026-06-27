//! GitHub-only login via the App's OAuth (user-to-server). `/auth/github/login`
//! sends the operator to GitHub's authorize page; `/auth/github/callback`
//! exchanges the code, upserts the user, opens a session, and sets an httpOnly
//! `mq_session` cookie. A session is an opaque row id looked up on each request
//! (see `current_user`, used by `GET /api/me`).
//!
//! The flow lives on `OAuthService` (`oauth`), persistence on `AuthStore`
//! (`store`), and failure modes on `AuthError` (`error`); the `#[handler]` fns
//! here only shape HTTP.

mod error;
mod oauth;
mod store;

pub use error::*;
pub use oauth::*;
pub use store::*;

use poem::http::StatusCode;
use poem::web::{Data, Query, Redirect};
use poem::{Response, Result, handler};
use sea_orm::{DatabaseConnection, DbErr};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;

const COOKIE: &str = "mq_session";

const FORBIDDEN_HTML: &str = "<!doctype html><html><head><meta charset=utf-8><title>not authorized</title></head>\
<body style=\"background:#0c0d10;color:#e6e4df;font-family:sans-serif;display:grid;place-items:center;height:100vh;text-align:center;margin:0\">\
<div style=\"max-width:42ch\"><h1 style=\"font-weight:800\">Not authorized</h1>\
<p style=\"color:#9498a1;line-height:1.5\">Your GitHub account doesn't have access to an organization where mergequeue is installed. Ask an admin to add you, then try again.</p>\
<a href=\"/\" style=\"color:#d99a4e;text-decoration:none\">← back</a></div></body></html>";

/// The authenticated user behind a session cookie, if the session is valid and
/// unexpired. Module-root delegator so callers resolve `crate::auth::current_user`;
/// the lookup lives on `AuthStore`.
pub async fn current_user(
    db: &DatabaseConnection,
    session_id: Uuid,
) -> Result<Option<user::Model>, DbErr> {
    AuthStore::current_user(db, session_id).await
}

/// `GET /auth/github/login` — redirect to GitHub's OAuth authorize page.
#[handler]
pub async fn login(cfg: Data<&Config>, db: Data<&DatabaseConnection>) -> Result<Redirect> {
    let url = OAuthService::initiate(&cfg, &db).await?;
    Ok(Redirect::see_other(url))
}

#[derive(Deserialize)]
pub struct AuthCallback {
    code: String,
    state: Option<String>,
}

/// `GET /auth/github/callback` — exchange the code, open a session, set the cookie.
#[handler]
pub async fn callback(
    Query(q): Query<AuthCallback>,
    cfg: Data<&Config>,
    db: Data<&DatabaseConnection>,
) -> Result<Response> {
    let state = q.state.as_deref().ok_or(AuthError::MissingState)?;
    let authed = match OAuthService::complete(&cfg, &db, &q.code, state).await {
        Ok(a) => a,
        Err(AuthError::NotAuthorized) => {
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .header("content-type", "text/html; charset=utf-8")
                .body(FORBIDDEN_HTML));
        }
        Err(e) => return Err(e.into()),
    };

    let session_id = AuthStore::create_session(&db, authed.user_pk)
        .await
        .map_err(AuthError::Db)?;
    tracing::info!(
        login = %authed.login,
        %session_id,
        "oauth callback: session created, setting cookie + redirecting to /app"
    );

    let cookie = format!(
        "{COOKIE}={session_id}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        AuthStore::SESSION_DAYS * 24 * 3600
    );
    Ok(Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header("Location", format!("{}/app", cfg.server.app_url()))
        .header("Set-Cookie", cookie)
        .finish())
}

/// `GET /auth/logout` — clear the session cookie and return to the app.
#[handler]
pub fn logout(cfg: Data<&Config>) -> Response {
    let cookie = format!("{COOKIE}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0");
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header("Location", cfg.server.app_url().to_owned())
        .header("Set-Cookie", cookie)
        .finish()
}
