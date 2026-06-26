//! GitHub-only login via the App's OAuth (user-to-server). `/auth/github/login`
//! sends the operator to GitHub's authorize page; `/auth/github/callback`
//! exchanges the code, upserts the user, opens a session, and sets an httpOnly
//! `mq_session` cookie. A session is an opaque row id looked up on each request
//! (see `current_user`, used by `GET /api/me`).

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use chrono::{Duration, Utc};
use poem::http::StatusCode;
use poem::web::{Data, Query, Redirect};
use poem::{Error, Response, Result, handler};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;
use crate::setup::resolve_credentials;

const COOKIE: &str = "mq_session";
const SESSION_DAYS: i64 = 30;

const FORBIDDEN_HTML: &str = "<!doctype html><html><head><meta charset=utf-8><title>not authorized</title></head>\
<body style=\"background:#0c0d10;color:#e6e4df;font-family:sans-serif;display:grid;place-items:center;height:100vh;text-align:center;margin:0\">\
<div style=\"max-width:42ch\"><h1 style=\"font-weight:800\">Not authorized</h1>\
<p style=\"color:#9498a1;line-height:1.5\">Your GitHub account doesn't have access to an organization where mergequeue is installed. Ask an admin to add you, then try again.</p>\
<a href=\"/\" style=\"color:#d99a4e;text-decoration:none\">← back</a></div></body></html>";

/// Short-lived CSRF state tokens for the OAuth round-trip (in-process).
static STATES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub mod user {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(unique)]
        pub github_id: i64,
        pub login: String,
        pub avatar_url: String,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod session {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "sessions")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_pk: Uuid,
        pub created_at: DateTimeWithTimeZone,
        pub expires_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

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

/// The authenticated user behind a session cookie, if the session is valid and unexpired.
pub async fn current_user(
    db: &DatabaseConnection,
    session_id: Uuid,
) -> Result<Option<user::Model>, DbErr> {
    let Some(s) = session::Entity::find_by_id(session_id).one(db).await? else {
        return Ok(None);
    };
    if s.expires_at.with_timezone(&Utc) < Utc::now() {
        return Ok(None);
    }
    user::Entity::find_by_id(s.user_pk).one(db).await
}

async fn upsert_user(
    db: &DatabaseConnection,
    github_id: i64,
    gh_login: &str,
    avatar_url: &str,
) -> Result<Uuid, DbErr> {
    user::Entity::insert(user::ActiveModel {
        id: Set(Uuid::new_v4()),
        github_id: Set(github_id),
        login: Set(gh_login.to_owned()),
        avatar_url: Set(avatar_url.to_owned()),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(user::Column::GithubId)
            .update_columns([user::Column::Login, user::Column::AvatarUrl])
            .to_owned(),
    )
    .exec(db)
    .await?;
    let row = user::Entity::find()
        .filter(user::Column::GithubId.eq(github_id))
        .one(db)
        .await?
        .ok_or_else(|| DbErr::Custom("user upsert lost".into()))?;
    Ok(row.id)
}

async fn create_session(db: &DatabaseConnection, user_pk: Uuid) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::days(SESSION_DAYS);
    session::ActiveModel {
        id: Set(id),
        user_pk: Set(user_pk),
        expires_at: Set(expires.fixed_offset()),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(id)
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
}

fn gateway(e: impl std::fmt::Display) -> Error {
    Error::from_string(e.to_string(), StatusCode::BAD_GATEWAY)
}

/// `GET /auth/github/login` — redirect to GitHub's OAuth authorize page.
#[handler]
pub async fn login(cfg: Data<&Config>, db: Data<&DatabaseConnection>) -> Result<Redirect> {
    let creds = resolve_credentials(&cfg, &db)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            Error::from_string("github app not set up", StatusCode::SERVICE_UNAVAILABLE)
        })?;
    let state = Uuid::new_v4().to_string();
    STATES.lock().unwrap().insert(state.clone());
    let redirect_uri = format!(
        "{}/auth/github/callback",
        cfg.server.app_url().trim_end_matches('/')
    );
    let url = reqwest::Url::parse_with_params(
        "https://github.com/login/oauth/authorize",
        &[
            ("client_id", creds.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", state.as_str()),
            ("scope", "read:user"),
            ("allow_signup", "false"),
        ],
    )
    .map_err(internal)?;
    Ok(Redirect::see_other(url.to_string()))
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
    let state = q
        .state
        .as_deref()
        .ok_or_else(|| Error::from_string("missing state", StatusCode::BAD_REQUEST))?;
    if !STATES.lock().unwrap().remove(state) {
        return Err(Error::from_string("unknown state", StatusCode::BAD_REQUEST));
    }
    let creds = resolve_credentials(&cfg, &db)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            Error::from_string("github app not set up", StatusCode::SERVICE_UNAVAILABLE)
        })?;
    let redirect_uri = format!(
        "{}/auth/github/callback",
        cfg.server.app_url().trim_end_matches('/')
    );

    let client = reqwest::Client::new();
    let token: TokenResponse = client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .header("User-Agent", "mergequeue")
        .json(&serde_json::json!({
            "client_id": creds.client_id,
            "client_secret": creds.client_secret,
            "code": q.code,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await
        .map_err(gateway)?
        .json()
        .await
        .map_err(gateway)?;
    let access = token
        .access_token
        .ok_or_else(|| Error::from_string("no access token", StatusCode::BAD_GATEWAY))?;

    let gh_user: GitHubUser = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {access}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "mergequeue")
        .send()
        .await
        .map_err(gateway)?
        .json()
        .await
        .map_err(gateway)?;

    // Authorization gate: only users who can access an installation of this App
    // (members of an installed org, or repo collaborators) may sign in. A user with
    // no accessible installation is authenticated by GitHub but not authorized here.
    let installs: UserInstallations = client
        .get("https://api.github.com/user/installations")
        .header("Authorization", format!("Bearer {access}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "mergequeue")
        .send()
        .await
        .map_err(gateway)?
        .json()
        .await
        .map_err(gateway)?;
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
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("content-type", "text/html; charset=utf-8")
            .body(FORBIDDEN_HTML));
    }

    let user_pk = upsert_user(
        &db,
        gh_user.id,
        &gh_user.login,
        gh_user.avatar_url.as_deref().unwrap_or(""),
    )
    .await
    .map_err(internal)?;
    let session_id = create_session(&db, user_pk).await.map_err(internal)?;
    tracing::info!(
        login = %gh_user.login,
        %session_id,
        "oauth callback: session created, setting cookie + redirecting to /app"
    );

    let cookie = format!(
        "{COOKIE}={session_id}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        SESSION_DAYS * 24 * 3600
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
