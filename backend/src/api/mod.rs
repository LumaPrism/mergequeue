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
use crate::store::{QueueSummary, RepoSummary, Store, queue_ledger};

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

/// A repo under management, with its named queues (the dashboard's switcher).
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct RepoView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub queues: Vec<QueueView>,
}

impl From<RepoSummary> for RepoView {
    fn from(r: RepoSummary) -> Self {
        Self {
            id: r.id.to_string(),
            owner: r.owner,
            name: r.name,
            queues: r.queues.into_iter().map(QueueView::from).collect(),
        }
    }
}

/// One named queue with its config + live depth. `active` is the active-batch
/// summary; it's populated by the per-repo queues endpoint and left `None` in the
/// lightweight repo switcher.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct QueueView {
    pub id: String,
    pub repo_id: String,
    pub name: String,
    pub base_branch: String,
    pub batch_size: i32,
    pub depth: i32,
    pub active: Option<ActiveBatchView>,
}

impl From<QueueSummary> for QueueView {
    fn from(q: QueueSummary) -> Self {
        Self {
            id: q.id.to_string(),
            repo_id: q.repo_id.to_string(),
            name: q.name,
            base_branch: q.base_branch,
            batch_size: q.batch_size,
            depth: q.queued as i32,
            active: None,
        }
    }
}

/// A compact summary of a queue's in-flight batch.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct ActiveBatchView {
    pub id: String,
    pub state: BatchState,
    #[typeshare(serialized_as = "Vec<u32>")]
    pub prs: Vec<u64>,
}

impl ActiveBatchView {
    fn of(batch: &BatchView) -> Self {
        Self {
            id: batch.id.to_string(),
            state: batch.state,
            prs: batch.prs(),
        }
    }
}

/// Create a queue: a name plus optional config overriding the repo's default queue.
#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct CreateQueueRequest {
    pub name: String,
    pub base_branch: Option<String>,
    pub batch_size: Option<i32>,
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

    fn parse_queue(raw: &str) -> Result<Uuid> {
        Uuid::parse_str(raw)
            .map_err(|_| Error::from_string("invalid queue id", StatusCode::BAD_REQUEST))
    }

    /// A queue name is ref-safe iff it matches `^[a-z0-9][a-z0-9-]*$` — it's folded
    /// directly into the staging ref.
    fn is_ref_safe(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
            _ => return false,
        }
        name.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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

    fn forbidden() -> Error {
        Error::from_string("forbidden", StatusCode::FORBIDDEN)
    }

    /// Whether `login` may write `repo_id` — the dashboard's write-authz gate,
    /// mirroring the PR-comment command path. Fails closed if the permission check
    /// itself errors, so a transient GitHub failure never grants access.
    async fn can_write_repo(&self, repo_id: Uuid, login: &str) -> bool {
        match self.rt.can_write(repo_id, login).await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, login, "repo permission check failed");
                false
            }
        }
    }

    /// Require a session AND write access to `repo_id`; returns the signed-in login.
    /// 403 when the user can't write the repo.
    async fn authorize_repo(
        &self,
        session: &Cookie<Option<String>>,
        repo_id: Uuid,
    ) -> Result<String> {
        let login = self.require_session(session).await?;
        if !self.can_write_repo(repo_id, &login).await {
            return Err(Self::forbidden());
        }
        Ok(login)
    }

    /// Require a session AND write access to the queue's repo; returns the signed-in
    /// login. 404 when the queue is unknown, 403 when the user can't write its repo.
    async fn authorize_queue(
        &self,
        session: &Cookie<Option<String>>,
        queue_id: Uuid,
    ) -> Result<String> {
        let login = self.require_session(session).await?;
        let repo_id = Store::queue_repo_id(&self.db, queue_id)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(|| Error::from_string("unknown queue", StatusCode::NOT_FOUND))?;
        if !self.can_write_repo(repo_id, &login).await {
            return Err(Self::forbidden());
        }
        Ok(login)
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

    /// A repo's named queues, each with its config, live depth, and active-batch
    /// summary (the per-repo queue switcher).
    #[oai(path = "/repos/:repo_id/queues", method = "get")]
    async fn list_queues(
        &self,
        repo_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<QueueView>>> {
        let repo_id = Self::parse_repo(&repo_id.0)?;
        self.authorize_repo(&session, repo_id).await?;
        let summaries = Store::list_queues(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let mut views = Vec::with_capacity(summaries.len());
        for s in summaries {
            let id = s.id;
            let mut view = QueueView::from(s);
            view.active = Store::active_batch_view(&self.db, id)
                .await
                .map_err(Self::db_err)?
                .as_ref()
                .map(ActiveBatchView::of);
            views.push(view);
        }
        Ok(Json(views))
    }

    /// Create a named queue on a repo. Optional `baseBranch`/`batchSize` override the
    /// repo's default queue config; everything else is inherited from it.
    #[oai(path = "/repos/:repo_id/queues", method = "post")]
    async fn create_queue(
        &self,
        repo_id: Path<String>,
        body: Json<CreateQueueRequest>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<QueueView>> {
        let repo_id = Self::parse_repo(&repo_id.0)?;
        self.authorize_repo(&session, repo_id).await?;
        let name = body.0.name.trim().to_lowercase();
        if !Self::is_ref_safe(&name) {
            return Err(Error::from_string(
                "queue name must match ^[a-z0-9][a-z0-9-]*$",
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
        if Store::queue_id_by_name(&self.db, repo_id, &name)
            .await
            .map_err(Self::db_err)?
            .is_some()
        {
            return Err(Error::from_string(
                format!("a queue named `{name}` already exists in this repo"),
                StatusCode::CONFLICT,
            ));
        }
        let default_id = Store::queue_id_by_name(&self.db, repo_id, "default")
            .await
            .map_err(Self::db_err)?
            .ok_or_else(|| Error::from_string("unknown repo", StatusCode::NOT_FOUND))?;
        let default = Store::queue_config(&self.db, default_id)
            .await
            .map_err(Self::db_err)?;
        let default_base = default.base_branch.clone();
        let base_branch = body.0.base_branch.unwrap_or(default.base_branch);
        let batch_size = body.0.batch_size.unwrap_or(default.batch_size as i32);
        if batch_size <= 0 {
            return Err(Error::from_string(
                "batch size must be a positive integer",
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
        let required_checks = if base_branch == default_base {
            default.required_checks
        } else {
            self.rt
                .required_checks(repo_id, &base_branch)
                .await
                .unwrap_or_default()
        };
        let queue_id = Store::create_queue(
            &self.db,
            repo_id,
            &name,
            &base_branch,
            batch_size,
            default.merge_method,
            &default.staging_prefix,
            &required_checks,
        )
        .await
        .map_err(Self::db_err)?;
        let summaries = Store::list_queues(&self.db, repo_id)
            .await
            .map_err(Self::db_err)?;
        let view = summaries
            .into_iter()
            .find(|s| s.id == queue_id)
            .map(QueueView::from)
            .ok_or_else(|| {
                Error::from_string("queue vanished", StatusCode::INTERNAL_SERVER_ERROR)
            })?;
        Ok(Json(view))
    }

    /// A queue's open entries, projected against its active batch (the train view).
    #[oai(path = "/queues/:queue_id", method = "get")]
    async fn get_queue(
        &self,
        queue_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<EntryView>>> {
        let queue_id = Self::parse_queue(&queue_id.0)?;
        self.authorize_queue(&session, queue_id).await?;
        let entries = Store::list_entries(&self.db, queue_id)
            .await
            .map_err(Self::db_err)?;
        let batch = Store::active_batch_view(&self.db, queue_id)
            .await
            .map_err(Self::db_err)?;
        let views = entries
            .into_iter()
            .map(|e| EntryView::project(e, batch.as_ref()))
            .collect();
        Ok(Json(views))
    }

    /// Recent finished batch runs for a queue, newest first (the history view).
    /// `limit` defaults to 50 and is capped at 200.
    #[oai(path = "/queues/:queue_id/ledger", method = "get")]
    async fn get_ledger(
        &self,
        queue_id: Path<String>,
        limit: Query<Option<u64>>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<LedgerView>>> {
        let queue_id = Self::parse_queue(&queue_id.0)?;
        self.authorize_queue(&session, queue_id).await?;
        let limit = limit.0.unwrap_or(50).min(200);
        let rows = Store::list_ledger(&self.db, queue_id, limit)
            .await
            .map_err(Self::db_err)?;
        Ok(Json(rows.into_iter().map(LedgerView::project).collect()))
    }

    /// Add a PR to a queue. Validates the PR's base matches the queue, and that the
    /// PR isn't already open in another queue of the repo, before accepting it.
    #[oai(path = "/queues/:queue_id/enqueue", method = "post")]
    async fn enqueue(
        &self,
        queue_id: Path<String>,
        body: Json<EnqueueRequest>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<EntryView>> {
        let queue_id = Self::parse_queue(&queue_id.0)?;
        let by = self.authorize_queue(&session, queue_id).await?;
        match self
            .rt
            .enqueue_pr(queue_id, body.0.pr_number as u64, &by)
            .await
            .map_err(Self::enqueue_err)?
        {
            Enqueued::Ok { entry, .. } => {
                let batch = Store::active_batch_view(&self.db, queue_id)
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
            Enqueued::AlreadyQueued { queue } => Err(Error::from_string(
                format!("this PR is already open in the `{queue}` queue; remove it there first"),
                StatusCode::CONFLICT,
            )),
        }
    }

    /// Remove a PR from a queue. Works whether it's queued or already testing in the
    /// active batch — a testing PR cancels its batch and re-queues the rest. The entry
    /// id is globally unique; its queue is resolved from the entry itself.
    #[oai(path = "/queues/:queue_id/entries/:entry_id", method = "delete")]
    async fn dequeue(
        &self,
        queue_id: Path<String>,
        entry_id: Path<String>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Health>> {
        let queue_id = Self::parse_queue(&queue_id.0)?;
        self.authorize_queue(&session, queue_id).await?;
        let entry_id = Self::parse_queue(&entry_id.0)?;
        if let Some(entry) = Store::entry(&self.db, entry_id)
            .await
            .map_err(Self::db_err)?
            && entry.queue_id != queue_id
        {
            return Err(Error::from_string("unknown entry", StatusCode::NOT_FOUND));
        }
        match self.rt.force_dequeue(entry_id).await.map_err(|_| {
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

    /// Reorder a queue's queued entries (drag-to-reorder the train). Only `queued`
    /// entries move; an entry already testing in the active batch keeps its slot.
    #[oai(path = "/queues/:queue_id/order", method = "put")]
    async fn reorder(
        &self,
        queue_id: Path<String>,
        body: Json<ReorderRequest>,
        #[oai(name = "mq_session")] session: Cookie<Option<String>>,
    ) -> Result<Json<Vec<EntryView>>> {
        let queue_id = Self::parse_queue(&queue_id.0)?;
        self.authorize_queue(&session, queue_id).await?;
        let mut ids = Vec::with_capacity(body.0.entry_ids.len());
        for raw in &body.0.entry_ids {
            ids.push(
                Uuid::parse_str(raw)
                    .map_err(|_| Error::from_string("invalid entry id", StatusCode::BAD_REQUEST))?,
            );
        }
        let txn = self.db.begin().await.map_err(Self::db_err)?;
        Store::reorder(&txn, queue_id, &ids)
            .await
            .map_err(Self::db_err)?;
        txn.commit().await.map_err(Self::db_err)?;
        let entries = Store::list_entries(&self.db, queue_id)
            .await
            .map_err(Self::db_err)?;
        let batch = Store::active_batch_view(&self.db, queue_id)
            .await
            .map_err(Self::db_err)?;
        let views = entries
            .into_iter()
            .map(|e| EntryView::project(e, batch.as_ref()))
            .collect();
        Ok(Json(views))
    }

    // TODO:
    //   GET /queues/:queue_id/batches          → active + recent batch history
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, LazyLock};

    use async_trait::async_trait;
    use chrono::{DateTime, Duration, Utc};
    use migration::{Migrator, MigratorTrait};
    use poem::http::StatusCode;
    use poem_openapi::param::{Cookie, Path, Query};
    use poem_openapi::payload::Json;
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Set,
        Statement,
    };
    use secrecy::SecretString;
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    use super::{Api, CreateQueueRequest};
    use crate::auth;
    use crate::config::{Config, DatabaseConfig, ServerConfig};
    use crate::github::{
        CheckState, GitHubError, MergeOutcome, PullSummary, RepoClient, RepoId, RepoPermission,
    };
    use crate::queue::LedgerOutcome;
    use crate::runtime::Runtime;
    use crate::store::{Store, queue_ledger};

    /// A minimal `RepoClient` for API authz tests: it answers `user_permission` with a
    /// fixed level and `required_checks` with a fixed set; everything else is inert.
    struct FakeRepo {
        permission: RepoPermission,
        required: Vec<String>,
    }

    impl FakeRepo {
        fn new(permission: RepoPermission) -> Self {
            Self {
                permission,
                required: vec![],
            }
        }

        fn with_required(mut self, required: &[&str]) -> Self {
            self.required = required.iter().map(|s| s.to_string()).collect();
            self
        }
    }

    #[async_trait]
    impl RepoClient for FakeRepo {
        async fn list_open_pulls(&self, _: &RepoId) -> Result<Vec<PullSummary>, GitHubError> {
            Ok(vec![])
        }
        async fn base_sha(&self, _: &RepoId, _: &str) -> Result<String, GitHubError> {
            Ok(String::new())
        }
        async fn pull(&self, _: &RepoId, pr: u64) -> Result<PullSummary, GitHubError> {
            Ok(PullSummary {
                number: pr,
                title: String::new(),
                head_sha: format!("head{pr}"),
                head_ref: format!("feature-{pr}"),
                base_ref: "main".into(),
                mergeable: Some(true),
                approved: false,
            })
        }
        async fn merge_onto(
            &self,
            _: &RepoId,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<MergeOutcome, GitHubError> {
            Ok(MergeOutcome::Merged)
        }
        async fn force_ref(&self, _: &RepoId, _: &str, _: &str) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn delete_ref(&self, _: &RepoId, _: &str) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn check_state(
            &self,
            _: &RepoId,
            _: &str,
            _: &[String],
        ) -> Result<CheckState, GitHubError> {
            Ok(CheckState::Pending)
        }
        async fn reported_contexts(&self, _: &RepoId, _: &str) -> Result<Vec<String>, GitHubError> {
            Ok(vec![])
        }
        async fn fast_forward(&self, _: &RepoId, _: &str, _: &str) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn comment(&self, _: &RepoId, _: u64, _: &str) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn ensure_label(
            &self,
            _: &RepoId,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn add_labels(&self, _: &RepoId, _: u64, _: &[String]) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn remove_label(&self, _: &RepoId, _: u64, _: &str) -> Result<(), GitHubError> {
            Ok(())
        }
        async fn user_permission(
            &self,
            _: &RepoId,
            _: &str,
        ) -> Result<RepoPermission, GitHubError> {
            Ok(self.permission)
        }
        async fn required_checks(&self, _: &RepoId, _: &str) -> Result<Vec<String>, GitHubError> {
            Ok(self.required.clone())
        }
    }

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
            "TRUNCATE queue_ledger, batch_entries, batches, queue_entries, queues, sessions, \
             users, repos, installations CASCADE",
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

    /// An `Api` with `client` installed so the write-authz gate (and branch-protection
    /// reads) resolve against it instead of an unconfigured App.
    async fn api_with_client(db: DatabaseConnection, client: FakeRepo) -> Api {
        let api = test_api(db);
        api.rt.install_test_repo_client(Arc::new(client)).await;
        api
    }

    async fn seed_repo(db: &DatabaseConnection) -> (Uuid, Uuid) {
        Store::provision_installation(db, 88, "acme").await.unwrap();
        Store::upsert_repo(db, 88, "acme", "widgets").await.unwrap();
        let repo_id = Store::repo_id_by_name(db, "acme", "widgets")
            .await
            .unwrap()
            .unwrap();
        let queue_id = Store::queue_id_by_name(db, repo_id, "default")
            .await
            .unwrap()
            .unwrap();
        (repo_id, queue_id)
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
        queue_id: Uuid,
        ended_at: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        queue_ledger::ActiveModel {
            id: Set(id),
            repo_id: Set(repo_id),
            queue_id: Set(queue_id),
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
        let api = api_with_client(db.clone(), FakeRepo::new(RepoPermission::Write)).await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let sid = seed_session(&db).await;
        let base = Utc::now();
        let older = insert_ledger(&db, repo_id, queue_id, base).await;
        let newer = insert_ledger(&db, repo_id, queue_id, base + Duration::seconds(5)).await;

        let views = api
            .get_ledger(
                Path(queue_id.to_string()),
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
        let (_repo_id, queue_id) = seed_repo(&db).await;

        let err = api
            .get_ledger(Path(queue_id.to_string()), Query(None), Cookie(None))
            .await
            .err()
            .expect("a missing session must be rejected");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_queue_route_forbids_non_writer() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = api_with_client(db.clone(), FakeRepo::new(RepoPermission::Read)).await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let sid = seed_session(&db).await;
        let base = Utc::now();
        insert_ledger(&db, repo_id, queue_id, base).await;

        let err = api
            .get_ledger(
                Path(queue_id.to_string()),
                Query(None),
                Cookie(Some(sid.to_string())),
            )
            .await
            .err()
            .expect("a read-only user must not read a queue they can't write");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_api_create_queue_forbids_non_writer() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = api_with_client(db.clone(), FakeRepo::new(RepoPermission::Read)).await;
        let (repo_id, _queue_id) = seed_repo(&db).await;
        let sid = seed_session(&db).await;

        let err = api
            .create_queue(
                Path(repo_id.to_string()),
                Json(CreateQueueRequest {
                    name: "frontend".into(),
                    base_branch: None,
                    batch_size: None,
                }),
                Cookie(Some(sid.to_string())),
            )
            .await
            .err()
            .expect("a read-only user must not create a queue");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_api_create_queue_rejects_duplicate_name() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = api_with_client(db.clone(), FakeRepo::new(RepoPermission::Write)).await;
        let (repo_id, _queue_id) = seed_repo(&db).await;
        let sid = seed_session(&db).await;
        let req = || {
            Json(CreateQueueRequest {
                name: "frontend".into(),
                base_branch: None,
                batch_size: Some(2),
            })
        };

        api.create_queue(
            Path(repo_id.to_string()),
            req(),
            Cookie(Some(sid.to_string())),
        )
        .await
        .expect("the first create succeeds");

        let err = api
            .create_queue(
                Path(repo_id.to_string()),
                req(),
                Cookie(Some(sid.to_string())),
            )
            .await
            .err()
            .expect("a duplicate queue name must be rejected, not silently absorbed");
        assert_eq!(err.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_api_create_queue_seeds_required_checks_from_new_base() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let api = api_with_client(
            db.clone(),
            FakeRepo::new(RepoPermission::Write).with_required(&["release-ci"]),
        )
        .await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let sid = seed_session(&db).await;
        Store::set_queue_required_checks(&db, default_id, &["ci".to_string()])
            .await
            .unwrap();

        let same = api
            .create_queue(
                Path(repo_id.to_string()),
                Json(CreateQueueRequest {
                    name: "same".into(),
                    base_branch: None,
                    batch_size: None,
                }),
                Cookie(Some(sid.to_string())),
            )
            .await
            .unwrap()
            .0;
        let same_cfg = Store::queue_config(&db, Uuid::parse_str(&same.id).unwrap())
            .await
            .unwrap();
        assert_eq!(
            same_cfg.required_checks,
            vec!["ci".to_string()],
            "a queue on the default base clones the default queue's checks"
        );

        let diff = api
            .create_queue(
                Path(repo_id.to_string()),
                Json(CreateQueueRequest {
                    name: "rel".into(),
                    base_branch: Some("release".into()),
                    batch_size: None,
                }),
                Cookie(Some(sid.to_string())),
            )
            .await
            .unwrap()
            .0;
        let diff_cfg = Store::queue_config(&db, Uuid::parse_str(&diff.id).unwrap())
            .await
            .unwrap();
        assert_eq!(
            diff_cfg.required_checks,
            vec!["release-ci".to_string()],
            "a queue on a different base seeds checks from that base's branch protection"
        );
        assert_eq!(diff_cfg.base_branch, "release");
    }
}
