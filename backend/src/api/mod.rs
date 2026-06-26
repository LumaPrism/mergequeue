//! REST API consumed by the Next.js dashboard. poem-openapi gives us a typed
//! spec + Swagger UI for free. Handlers delegate to the
//! connection-generic `Store`; the engine runs out of band in the worker.

use std::sync::Arc;

use poem::http::StatusCode;
use poem::{Error, Result};
use poem_openapi::param::{Cookie, Path, Query};
use poem_openapi::payload::Json;
use poem_openapi::{Enum, Object, OpenApi};
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde::{Deserialize, Serialize};
use typeshare::typeshare;
use uuid::Uuid;

use crate::auth::current_user;
use crate::config::Config;
use crate::error::Error as AppError;
use crate::github::PullSummary;
use crate::queue::{
    BatchState, BatchView, EntryState, LedgerEntryResult, LedgerOutcome, QueueEntry,
};
use crate::runtime::{AppOwner, Enqueued, Removed, Runtime};
use crate::setup::resolve_credentials;
use crate::store::{RepoSummary, Store, queue_ledger};

pub struct Api {
    pub cfg: Config,
    pub db: DatabaseConnection,
    pub rt: Arc<Runtime>,
}

#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct Health {
    pub status: String,
}

#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct EntryView {
    pub id: String,
    pub pr_number: u32,
    pub position: i32,
    pub status: PrStatus,
}

/// A PR's place in the merge lifecycle — projected from its entry + batch.
#[typeshare]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Enum)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
pub enum PrStatus {
    Queued,
    Testing,
    Merging,
    Blocked,
    Merged,
    Ejected,
}

impl PrStatus {
    fn of(state: EntryState, batch: Option<&BatchView>) -> Self {
        match state {
            EntryState::Queued => Self::Queued,
            EntryState::Merged => Self::Merged,
            EntryState::Ejected => Self::Ejected,
            EntryState::Testing => match batch {
                Some(b) if b.merge_blocked => Self::Blocked,
                Some(b) if b.state == BatchState::Merging => Self::Merging,
                _ => Self::Testing,
            },
        }
    }
}

impl EntryView {
    fn project(e: QueueEntry, batch: Option<&BatchView>) -> Self {
        Self {
            status: PrStatus::of(e.state, batch),
            id: e.id.to_string(),
            pr_number: e.pr_number as u32,
            position: e.position,
        }
    }
}

/// One PR's fate within a finished batch run, projected from the ledger.
#[typeshare]
#[derive(Serialize, Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct LedgerEntryView {
    #[typeshare(serialized_as = "u32")]
    pub pr_number: u64,
    pub result: LedgerEntryResult,
}

/// One finished batch run from the append-only ledger (the dashboard's history view).
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct LedgerView {
    pub id: String,
    pub batch_id: String,
    pub outcome: LedgerOutcome,
    pub base_sha: String,
    pub landed_sha: Option<String>,
    #[typeshare(serialized_as = "Option<u32>")]
    pub ejected_pr: Option<u64>,
    pub entries: Vec<LedgerEntryView>,
    pub started_at: String,
    pub ended_at: String,
}

impl LedgerView {
    fn project(m: queue_ledger::Model) -> Self {
        Self {
            id: m.id.to_string(),
            batch_id: m.batch_id.to_string(),
            outcome: m.outcome,
            base_sha: m.base_sha,
            landed_sha: m.landed_sha,
            ejected_pr: m.ejected_pr.map(|p| p as u64),
            entries: serde_json::from_value(m.entries).unwrap_or_default(),
            started_at: m.started_at.to_rfc3339(),
            ended_at: m.ended_at.to_rfc3339(),
        }
    }
}

/// A repo under management, with its live queue depth.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct RepoView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub base_branch: String,
    pub batch_size: i32,
    pub queued: i32,
}

impl From<RepoSummary> for RepoView {
    fn from(r: RepoSummary) -> Self {
        Self {
            id: r.id.to_string(),
            owner: r.owner,
            name: r.name,
            base_branch: r.base_branch,
            batch_size: r.batch_size,
            queued: r.queued as i32,
        }
    }
}

#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct EnqueueRequest {
    pub pr_number: u32,
}

/// Drag-to-reorder: the queued entry ids in their new order (front of the train first).
#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct ReorderRequest {
    pub entry_ids: Vec<String>,
}

/// The signed-in GitHub user, for the dashboard's auth gate.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct MeView {
    pub login: String,
    pub avatar_url: String,
}

/// An open PR — a candidate to add to the queue.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct PrView {
    pub number: u32,
    pub title: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: Option<bool>,
}

impl From<PullSummary> for PrView {
    fn from(p: PullSummary) -> Self {
        Self {
            number: p.number as u32,
            title: p.title,
            head_ref: p.head_ref,
            base_ref: p.base_ref,
            mergeable: p.mergeable,
        }
    }
}

/// Where the resolved GitHub App credentials came from.
#[typeshare]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
pub enum SetupSource {
    /// Static credentials from config/env (the static escape hatch).
    Config,
    /// Credentials minted by the `/setup` manifest flow, stored in the DB.
    Manifest,
}

/// Whether the GitHub App is registered. Drives the dashboard's setup gate so the
/// manifest flow is offered only when the App is missing.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct SetupStatus {
    pub registered: bool,
    /// Where to start the manifest flow (always present, used by the "connect" CTA).
    pub setup_url: String,
    pub slug: Option<String>,
    pub install_url: Option<String>,
    pub manage_url: Option<String>,
    pub source: Option<SetupSource>,
}

impl Api {
    fn parse_repo(raw: &str) -> Result<Uuid> {
        Uuid::parse_str(raw)
            .map_err(|_| Error::from_string("invalid repo id", StatusCode::BAD_REQUEST))
    }

    fn db_err(e: sea_orm::DbErr) -> Error {
        Error::from_string(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Map an internal enqueue error to a client response without leaking internals:
    /// a GitHub-side failure is a 502, anything else a 500.
    fn enqueue_err(e: AppError) -> Error {
        match e {
            AppError::GitHub(_) => Error::from_string(
                "could not reach GitHub to validate this PR",
                StatusCode::BAD_GATEWAY,
            ),
            AppError::Config(_) | AppError::Db(_) | AppError::Engine(_) | AppError::Io(_) => {
                Error::from_string("could not queue this PR", StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }

    fn unauthorized() -> Error {
        Error::from_string("unauthorized", StatusCode::UNAUTHORIZED)
    }

    /// The App's GitHub settings URL, or `None` when the owner is unknown (an unknown
    /// owner must NOT fall back to the personal URL — that 404s for org-owned Apps).
    /// Org-owned Apps live under `/organizations/{login}/settings/apps/...`.
    fn manage_url(owner: Option<&AppOwner>, slug: &str) -> Option<String> {
        let o = owner?;
        Some(if o.is_org {
            format!(
                "https://github.com/organizations/{}/settings/apps/{slug}",
                o.login
            )
        } else {
            format!("https://github.com/settings/apps/{slug}")
        })
    }

    /// Require a valid `mq_session` cookie; returns the signed-in user's login.
    async fn require_session(&self, session: &Cookie<Option<String>>) -> Result<String> {
        let token = session.0.as_deref().ok_or_else(Self::unauthorized)?;
        let sid = Uuid::parse_str(token).map_err(|_| Self::unauthorized())?;
        let user = current_user(&self.db, sid)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(Self::unauthorized)?;
        Ok(user.login)
    }
}

#[OpenApi]
impl Api {
    #[oai(path = "/health", method = "get")]
    async fn health(&self) -> Json<Health> {
        Json(Health {
            status: "ok".into(),
        })
    }

    /// Is the GitHub App registered yet? The dashboard gates the manifest setup
    /// flow on this so the "connect" button shows only when the App is missing.
    #[oai(path = "/setup/status", method = "get")]
    async fn setup_status(&self) -> Json<SetupStatus> {
        let setup_url = format!("{}/setup", self.cfg.server.base_url.trim_end_matches('/'));
        let creds = resolve_credentials(&self.cfg, &self.db)
            .await
            .ok()
            .flatten();
        let owner = self.rt.app_owner().await;
        Json(match creds {
            Some(c) => SetupStatus {
                registered: true,
                setup_url,
                install_url: Some(format!(
                    "https://github.com/apps/{}/installations/new",
                    c.slug
                )),
                manage_url: Self::manage_url(owner.as_ref(), &c.slug),
                slug: Some(c.slug),
                source: Some(if self.cfg.github.is_some() {
                    SetupSource::Config
                } else {
                    SetupSource::Manifest
                }),
            },
            None => SetupStatus {
                registered: false,
                setup_url,
                slug: None,
                install_url: None,
                manage_url: None,
                source: None,
            },
        })
    }

    /// The signed-in user (or 401). The dashboard gates on this.
    #[oai(path = "/me", method = "get")]
    async fn me(
        &self,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<MeView>> {
        let unauthorized = || Error::from_string("unauthorized", StatusCode::UNAUTHORIZED);
        let token = session.0.ok_or_else(unauthorized)?;
        let sid = Uuid::parse_str(&token).map_err(|_| unauthorized())?;
        let user = current_user(&self.db, sid)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(unauthorized)?;
        Ok(Json(MeView {
            login: user.login,
            avatar_url: user.avatar_url,
        }))
    }

    /// Repos under management, with live queue depth (the dashboard's switcher).
    #[oai(path = "/repos", method = "get")]
    async fn list_repos(
        &self,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<RepoView>>> {
        self.require_session(&session).await?;
        let repos = Store::list_repos(&self.db).await.map_err(Self::db_err)?;
        Ok(Json(repos.into_iter().map(RepoView::from).collect()))
    }

    /// Open PRs for a repo — queue candidates the dashboard lists with a Queue button.
    #[oai(path = "/repos/:repo_id/prs", method = "get")]
    async fn list_prs(
        &self,
        repo_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<PrView>>> {
        self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        let pulls = self
            .rt
            .list_open_pulls(repo_id)
            .await
            .map_err(|e| Error::from_string(e.to_string(), StatusCode::BAD_GATEWAY))?;
        Ok(Json(pulls.into_iter().map(PrView::from).collect()))
    }

    /// Current queue for a repo (the dashboard's main view).
    #[oai(path = "/repos/:repo_id/queue", method = "get")]
    async fn get_queue(
        &self,
        repo_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<EntryView>>> {
        self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        let entries = Store::list_entries(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let batch = Store::active_batch_view(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let views = entries
            .into_iter()
            .map(|e| EntryView::project(e, batch.as_ref()))
            .collect();
        Ok(Json(views))
    }

    /// Recent finished batch runs for a repo, newest first (the dashboard's history
    /// view). `limit` defaults to 50 and is capped at 200.
    #[oai(path = "/repos/:repo_id/ledger", method = "get")]
    async fn get_ledger(
        &self,
        repo_id: Path<String>,
        limit: Query<Option<u64>>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<LedgerView>>> {
        self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        let limit = limit.0.unwrap_or(50).min(200);
        let rows = Store::list_ledger(&self.db, repo_id, limit)
            .await
            .map_err(Self::db_err)?;
        Ok(Json(rows.into_iter().map(LedgerView::project).collect()))
    }

    /// Add a PR to the queue. Validates the PR's base matches the queue before
    /// accepting it (a PR into another branch is rejected, not merged into base).
    #[oai(path = "/repos/:repo_id/queue", method = "post")]
    async fn enqueue(
        &self,
        repo_id: Path<String>,
        body: Json<EnqueueRequest>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<EntryView>> {
        let by = self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        match self
            .rt
            .enqueue_pr(repo_id, body.0.pr_number as u64, &by)
            .await
            .map_err(Self::enqueue_err)?
        {
            Enqueued::Ok { entry, .. } => {
                let batch = Store::active_batch_view(&self.db, repo_id)
                    .await
                    .map_err(Self::db_err)?;
                Ok(Json(EntryView::project(entry, batch.as_ref())))
            }
            Enqueued::WrongBase {
                pr_base,
                queue_base,
            } => Err(Error::from_string(
                format!("this PR targets {pr_base}, but the queue lands into {queue_base}"),
                StatusCode::UNPROCESSABLE_ENTITY,
            )),
        }
    }

    /// Remove a PR from the train. Works whether it's queued or already testing in
    /// the active batch — a testing PR cancels its batch and re-queues the rest.
    #[oai(path = "/repos/:repo_id/queue/:entry_id", method = "delete")]
    async fn dequeue(
        &self,
        repo_id: Path<String>,
        entry_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Health>> {
        self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        let entry_id = Self::parse_repo(&entry_id.0)?;
        match self
            .rt
            .force_dequeue(repo_id, entry_id)
            .await
            .map_err(|_| {
                Error::from_string(
                    "could not remove this PR",
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })? {
            Removed::Busy { .. } => Err(Error::from_string(
                "can't remove — the batch is merging; try again in a moment",
                StatusCode::CONFLICT,
            )),
            Removed::Gone { .. } | Removed::NotQueued => Ok(Json(Health {
                status: "ok".into(),
            })),
        }
    }

    /// Reorder the queued entries (drag-to-reorder the train). Only `queued`
    /// entries move; an entry already testing in the active batch keeps its slot.
    #[oai(path = "/repos/:repo_id/queue/order", method = "put")]
    async fn reorder(
        &self,
        repo_id: Path<String>,
        body: Json<ReorderRequest>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<EntryView>>> {
        self.require_session(&session).await?;
        let repo_id = Self::parse_repo(&repo_id.0)?;
        let mut ids = Vec::with_capacity(body.0.entry_ids.len());
        for raw in &body.0.entry_ids {
            ids.push(
                Uuid::parse_str(raw)
                    .map_err(|_| Error::from_string("invalid entry id", StatusCode::BAD_REQUEST))?,
            );
        }
        let txn = self.db.begin().await.map_err(Self::db_err)?;
        Store::reorder(&txn, repo_id, &ids)
            .await
            .map_err(Self::db_err)?;
        txn.commit().await.map_err(Self::db_err)?;
        let entries = Store::list_entries(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let batch = Store::active_batch_view(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let views = entries
            .into_iter()
            .map(|e| EntryView::project(e, batch.as_ref()))
            .collect();
        Ok(Json(views))
    }

    // TODO:
    //   GET /repos/:repo_id/batches            → active + recent batch history
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, LazyLock};

    use chrono::{DateTime, Duration, Utc};
    use migration::{Migrator, MigratorTrait};
    use poem::http::StatusCode;
    use poem_openapi::param::{Cookie, Path, Query};
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Set,
        Statement,
    };
    use secrecy::SecretString;
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    use super::Api;
    use crate::auth;
    use crate::config::{Config, DatabaseConfig, ServerConfig};
    use crate::queue::LedgerOutcome;
    use crate::runtime::Runtime;
    use crate::store::{Store, queue_ledger};

    /// Serializes DB tests against the shared test database.
    static DB_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    async fn test_db() -> DatabaseConnection {
        let url = std::env::var("MQ_TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5433/mergequeue_test".into()
        });
        let (host, _) = url.rsplit_once('/').expect("db url has a path");
        if let Ok(maint) = Database::connect(format!("{host}/postgres")).await {
            let _ = maint
                .execute(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "CREATE DATABASE mergequeue_test",
                ))
                .await;
        }
        let db = Database::connect(&url).await.expect("connect test db");
        Migrator::up(&db, None).await.expect("migrate test db");
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "TRUNCATE queue_ledger, batch_entries, batches, queue_entries, sessions, users, repos, \
             installations CASCADE",
        ))
        .await
        .unwrap();
        db
    }

    fn test_api(db: DatabaseConnection) -> Api {
        let cfg = Config {
            github: None,
            server: ServerConfig {
                base_url: "http://localhost:8080".into(),
                public_url: None,
                app_url: None,
                port: 8080,
            },
            database: DatabaseConfig {
                url: SecretString::from("postgres://test"),
            },
        };
        let rt = Arc::new(Runtime::new(cfg.clone(), db.clone()));
        Api { cfg, db, rt }
    }

    async fn seed_repo(db: &DatabaseConnection) -> Uuid {
        Store::provision_installation(db, 88, "acme").await.unwrap();
        Store::upsert_repo(db, 88, "acme", "widgets").await.unwrap();
        Store::repo_id_by_name(db, "acme", "widgets")
            .await
            .unwrap()
            .unwrap()
    }

    async fn seed_session(db: &DatabaseConnection) -> Uuid {
        let user_pk = Uuid::new_v4();
        auth::user::ActiveModel {
            id: Set(user_pk),
            github_id: Set(4242),
            login: Set("octocat".into()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        let sid = Uuid::new_v4();
        auth::session::ActiveModel {
            id: Set(sid),
            user_pk: Set(user_pk),
            expires_at: Set((Utc::now() + Duration::days(1)).into()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        sid
    }

    async fn insert_ledger(
        db: &DatabaseConnection,
        repo_id: Uuid,
        ended_at: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        queue_ledger::ActiveModel {
            id: Set(id),
            repo_id: Set(repo_id),
            batch_id: Set(Uuid::new_v4()),
            outcome: Set(LedgerOutcome::Merged),
            base_sha: Set("base000".into()),
            landed_sha: Set(Some("stg".into())),
            ejected_pr: Set(None),
            entries: Set(serde_json::json!([{"prNumber": 7, "result": "landed"}])),
            started_at: Set(ended_at.into()),
            ended_at: Set(ended_at.into()),
        }
        .insert(db)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn test_api_ledger_returns_rows_newest_first_for_session() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = test_api(db.clone());
        let repo_id = seed_repo(&db).await;
        let sid = seed_session(&db).await;
        let base = Utc::now();
        let older = insert_ledger(&db, repo_id, base).await;
        let newer = insert_ledger(&db, repo_id, base + Duration::seconds(5)).await;

        let views = api
            .get_ledger(
                Path(repo_id.to_string()),
                Query(None),
                Cookie(Some(sid.to_string())),
            )
            .await
            .unwrap()
            .0;
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].id, newer.to_string(), "newest run first");
        assert_eq!(views[1].id, older.to_string());
        assert_eq!(views[0].entries.len(), 1);
        assert_eq!(views[0].entries[0].pr_number, 7);
    }

    #[tokio::test]
    async fn test_api_ledger_rejects_missing_session() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = test_api(db.clone());
        let repo_id = seed_repo(&db).await;

        let err = api
            .get_ledger(Path(repo_id.to_string()), Query(None), Cookie(None))
            .await
            .err()
            .expect("a missing session must be rejected");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
}
