//! REST API consumed by the Next.js dashboard. poem-openapi gives us a typed
//! spec + Swagger UI for free. Handlers delegate to the
//! connection-generic `Store`; the engine runs out of band in the worker.

use std::sync::Arc;

use poem::http::StatusCode;
use poem::{Error, Result};
use poem_openapi::param::{Cookie, Path};
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
use crate::queue::{BatchState, BatchView, EntryState, QueueEntry};
use crate::runtime::{AppOwner, Enqueued, Removed, Runtime};
use crate::setup::resolve_credentials;
use crate::store::{RepoSummary, Store};

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
