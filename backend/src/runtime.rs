//! Hot-swappable runtime state: the engine and the webhook secret, held behind
//! shared cells so the `/setup` manifest flow can register the App and have the
//! instance start processing immediately — no restart. `reinit` resolves the
//! credentials and rebuilds the engine; the worker reads `engine()` each tick and
//! the webhook reads the shared secret cell.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait, Value,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::Config;
use crate::error::Result;
use crate::github::{
    AppClient, GitHubError, GitHubRepoClient, PullSummary, RepoClient, RepoId, TrainLabel,
};
use crate::queue::{BatchState, Engine, EntryState, QueueEntry};
use crate::setup::resolve_credentials;
use crate::store::Store;

/// Shared, late-initialized webhook secret. The webhook handler and the runtime
/// hold the same cell, so a post-startup `/setup` makes the secret live at once.
pub type SecretCell = Arc<RwLock<Option<SecretString>>>;

/// The account a GitHub App is owned by — determines its settings URL (an
/// org-owned app lives at `/organizations/{login}/settings/apps/...`, not
/// `/settings/apps/...`).
#[derive(Clone, Debug)]
pub struct AppOwner {
    pub login: String,
    pub is_org: bool,
}

#[derive(Clone)]
pub struct Runtime {
    cfg: Config,
    db: DatabaseConnection,
    engine: Arc<RwLock<Option<Arc<Engine>>>>,
    webhook_secret: SecretCell,
    app: Arc<RwLock<Option<AppClient>>>,
    repo_client: Arc<RwLock<Option<Arc<dyn RepoClient>>>>,
    app_owner: Arc<RwLock<Option<AppOwner>>>,
}

impl Runtime {
    pub fn new(cfg: Config, db: DatabaseConnection) -> Self {
        Self {
            cfg,
            db,
            engine: Arc::new(RwLock::new(None)),
            webhook_secret: Arc::new(RwLock::new(None)),
            app: Arc::new(RwLock::new(None)),
            repo_client: Arc::new(RwLock::new(None)),
            app_owner: Arc::new(RwLock::new(None)),
        }
    }

    /// The account that owns the GitHub App (login + whether it's an org), fetched
    /// once from `GET /app` and cached. The App's settings URL depends on it — an
    /// org-owned App lives under `/organizations/{login}/settings/apps/...`.
    pub async fn app_owner(&self) -> Option<AppOwner> {
        if let Some(o) = self.app_owner.read().await.clone() {
            return Some(o);
        }
        let app = self.app.read().await.clone()?;
        let info: AppInfo = app.app().get("/app", None::<&()>).await.ok()?;
        let owner = AppOwner {
            is_org: info.owner.kind == "Organization",
            login: info.owner.login,
        };
        *self.app_owner.write().await = Some(owner.clone());
        Some(owner)
    }

    pub fn db(&self) -> DatabaseConnection {
        self.db.clone()
    }

    /// The shared webhook-secret cell, handed to the webhook handler.
    pub fn secret_cell(&self) -> SecretCell {
        self.webhook_secret.clone()
    }

    /// The current engine, if the App is configured.
    pub async fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.read().await.clone()
    }

    /// Open PRs for a repo (queue candidates). Empty if the App isn't configured yet.
    pub async fn list_open_pulls(&self, repo_id: Uuid) -> Result<Vec<PullSummary>> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(vec![]);
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        Ok(client.list_open_pulls(&gh).await?)
    }

    /// Whether `username` has write (or admin) access to the repo — the authz gate
    /// for PR-comment queue commands. `false` when the App isn't configured.
    pub async fn can_write(&self, repo_id: Uuid, username: &str) -> Result<bool> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(false);
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        Ok(client.user_permission(&gh, username).await?.can_write())
    }

    /// Post a comment on a PR (the PR-comment command's reply). No-op when the App
    /// isn't configured.
    pub async fn comment(&self, repo_id: Uuid, pr: u64, body: &str) -> Result<()> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(());
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        client.comment(&gh, pr, body).await?;
        Ok(())
    }

    /// Validate and enqueue a PR — the single enqueue path for both the dashboard
    /// API and the `/mergequeue queue` PR-comment command. Fetches the PR live to
    /// reject one whose base doesn't match the queue (a backport/release PR must
    /// never be merged into the wrong base), then assigns its queue position under a
    /// per-repo advisory lock so concurrent enqueues can't collide on order.
    pub async fn enqueue_pr(&self, repo_id: Uuid, pr: u64, by: &str) -> Result<Enqueued> {
        let cfg = Store::repo_config(&self.db, repo_id).await?;
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        let Some(client) = self.repo_client.read().await.clone() else {
            return Err(GitHubError::Other("github app not configured".into()).into());
        };
        let summary = client.pull(&gh, pr).await?;
        if summary.base_ref != cfg.base_branch {
            return Ok(Enqueued::WrongBase {
                pr_base: summary.base_ref,
                queue_base: cfg.base_branch,
            });
        }
        let txn = self.db.begin().await?;
        let bytes = repo_id.as_bytes();
        let key = i64::from_le_bytes(bytes[..8].try_into().unwrap())
            ^ i64::from_le_bytes(bytes[8..].try_into().unwrap());
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [Value::from(key)],
        ))
        .await?;
        let entry = Store::enqueue(&txn, repo_id, pr, &summary.head_sha, by).await?;
        let position = Store::queue_rank(&txn, repo_id, entry.position).await? as i32;
        txn.commit().await?;
        let warn_no_checks = cfg.required_checks.is_empty();
        let mut comment = format!("**mergequeue** · queued · position {position}");
        if warn_no_checks {
            comment.push_str(" · ⚠️ no required checks configured (held)");
        }
        let _ = self.comment(repo_id, pr, &comment).await;
        let _ = client
            .set_train_label(&gh, pr, Some(TrainLabel::Queued))
            .await;
        Ok(Enqueued::Ok {
            entry,
            position,
            warn_no_checks,
        })
    }

    /// Best-effort merge-train label on a PR, set outside the engine loop (dashboard
    /// dequeue, PR close). `None` clears the label when a PR leaves the train.
    pub async fn set_pr_label(
        &self,
        repo_id: Uuid,
        pr: u64,
        target: Option<TrainLabel>,
    ) -> Result<()> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(());
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        client.set_train_label(&gh, pr, target).await?;
        Ok(())
    }

    /// Remove a PR from the train whatever its state. A queued entry is just
    /// deleted; a PR testing in — or wedged merging on — the active batch cancels
    /// that batch (drops the staging branch, re-queues its siblings) so the train
    /// rebuilds without it. The one case left untouched is a merge that has already
    /// fast-forwarded base to the staging tip: those PRs are effectively landed and
    /// must not be yanked, so that's reported as `Busy` (also when we can't confirm,
    /// to fail safe).
    pub async fn force_dequeue(&self, repo_id: Uuid, entry_id: Uuid) -> Result<Removed> {
        let Some(pr) = Store::entry_pr_number(&self.db, entry_id).await? else {
            return Ok(Removed::NotQueued);
        };
        match Store::open_entry_state(&self.db, repo_id, pr).await? {
            Some(EntryState::Queued) => {
                if !Store::dequeue(&self.db, repo_id, entry_id).await? {
                    return Ok(Removed::NotQueued);
                }
            }
            Some(EntryState::Testing) => {
                let Some(batch) = Store::active_batch(&self.db, repo_id).await? else {
                    return Ok(Removed::NotQueued);
                };
                if !batch.entry_ids.contains(&entry_id) {
                    return Ok(Removed::NotQueued);
                }
                if matches!(batch.state, BatchState::Merging) {
                    let staging_sha = batch.staging_sha.clone().unwrap_or_default();
                    let landed = match self.repo_client.read().await.clone() {
                        Some(client) => {
                            let cfg = Store::repo_config(&self.db, repo_id).await?;
                            let gh = Store::repo_ref(&self.db, repo_id).await?;
                            client
                                .base_sha(&gh, &cfg.base_branch)
                                .await
                                .map(|base| base == staging_sha)
                                .unwrap_or(true)
                        }
                        None => true,
                    };
                    if landed {
                        return Ok(Removed::Busy { pr });
                    }
                }
                let others: Vec<Uuid> = batch
                    .entry_ids
                    .iter()
                    .copied()
                    .filter(|id| *id != entry_id)
                    .collect();
                if let Some(client) = self.repo_client.read().await.clone() {
                    let gh = Store::repo_ref(&self.db, repo_id).await?;
                    let _ = client.delete_ref(&gh, &batch.staging_ref).await;
                }
                let txn = self.db.begin().await?;
                Store::remove_entry(&txn, repo_id, entry_id).await?;
                Store::requeue_entries(&txn, &others).await?;
                Store::set_batch_state(&txn, batch.id, BatchState::Superseded).await?;
                txn.commit().await?;
                for opr in Store::entry_prs(&self.db, &others)
                    .await
                    .unwrap_or_default()
                {
                    let _ = self
                        .set_pr_label(repo_id, opr, Some(TrainLabel::Queued))
                        .await;
                }
            }
            Some(EntryState::Merged) | Some(EntryState::Ejected) | None => {
                return Ok(Removed::NotQueued);
            }
        }
        let _ = self
            .comment(repo_id, pr, "**mergequeue** · removed from the train")
            .await;
        let _ = self.set_pr_label(repo_id, pr, None).await;
        Ok(Removed::Gone { pr })
    }

    /// Resolve credentials and (re)build the engine + webhook secret. Returns
    /// whether the App is now configured. Idempotent; called at startup and again
    /// from the `/setup` callback so registration takes effect without a restart.
    pub async fn reinit(&self) -> Result<bool> {
        let Some(creds) = resolve_credentials(&self.cfg, &self.db).await? else {
            return Ok(false);
        };
        let secret = SecretString::new(creds.webhook_secret.clone().into_boxed_str());
        let app_client = AppClient::from_credentials(creds)?;
        let repo_client: Arc<dyn RepoClient> = Arc::new(GitHubRepoClient::new(app_client.clone()));
        let engine = Arc::new(Engine::new(repo_client.clone(), self.db.clone()));
        *self.engine.write().await = Some(engine);
        *self.webhook_secret.write().await = Some(secret);
        *self.app.write().await = Some(app_client);
        *self.repo_client.write().await = Some(repo_client);
        *self.app_owner.write().await = None;
        Ok(true)
    }

    /// Reconcile installations + their repos from the App API — a backfill for any
    /// missed `installation`/`installation_repositories` webhook, so an installed
    /// repo always appears (including after a downtime install). Idempotent.
    pub async fn sync_installations(&self) -> Result<()> {
        let Some(app) = self.app.read().await.clone() else {
            return Ok(());
        };
        // Snapshot the moment before fetching: pruning only removes rows that predate
        // this, so anything a webhook provisions concurrently is never deleted.
        let snapshot = Utc::now().fixed_offset();
        let repo_client = self.repo_client.read().await.clone();
        let installs = Self::fetch_installations(&app).await?;
        let mut keep_installations: Vec<i64> = Vec::with_capacity(installs.len());
        for inst in &installs {
            Store::provision_installation(&self.db, inst.id, &inst.account.login).await?;
            keep_installations.push(inst.id);
            let repos = Self::fetch_repos(&app, inst.id).await?;
            let mut keep_repos: Vec<String> = Vec::with_capacity(repos.len());
            for repo in &repos {
                Store::upsert_repo(&self.db, inst.id, &repo.owner.login, &repo.name).await?;
                keep_repos.push(repo.name.clone());
                let Some(repo_id) =
                    Store::repo_id_by_name(&self.db, &repo.owner.login, &repo.name).await?
                else {
                    continue;
                };
                // Track the repo's GitHub default branch as the queue base, and mirror
                // its branch-protection required checks so the engine gates on the
                // repo's *real* required checks (an empty set holds the queue — that's
                // the safety guard, not a misconfiguration). Only overwrite the checks
                // on a successful read, so a transient API error never wipes them.
                Store::set_base_branch(&self.db, repo_id, &repo.default_branch).await?;
                if let Some(client) = &repo_client {
                    let gh = RepoId {
                        owner: repo.owner.login.clone(),
                        name: repo.name.clone(),
                        installation_id: inst.id as u64,
                    };
                    match client.required_checks(&gh, &repo.default_branch).await {
                        Ok(checks) => {
                            Store::set_required_checks(&self.db, repo_id, &checks).await?;
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            repo = %format!("{}/{}", repo.owner.login, repo.name),
                            "could not read branch protection — grant the App \
                             administration:read and re-approve the install; required checks \
                             left unchanged (the repo stays held if it has none yet)"
                        ),
                    }
                }
            }
            // drop repos this installation no longer grants access to.
            Store::prune_installation_repos(&self.db, inst.id, &keep_repos, snapshot).await?;
        }
        // drop installations the App was removed from (cascades their repos).
        Store::prune_installations(&self.db, &keep_installations, snapshot).await?;
        Ok(())
    }

    /// All installations of the App (paginated).
    async fn fetch_installations(app: &AppClient) -> Result<Vec<InstallationRow>> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let batch: Vec<InstallationRow> = app
                .app()
                .get("/app/installations", Some(&Pagination::page(page)))
                .await
                .map_err(GitHubError::from)?;
            let last = batch.len() < Pagination::PER_PAGE as usize;
            all.extend(batch);
            if last {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    /// All repositories one installation grants access to (paginated).
    async fn fetch_repos(app: &AppClient, installation_id: i64) -> Result<Vec<RepoRow>> {
        let client = app.installation(installation_id as u64)?;
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let resp: RepoListResponse = client
                .get("/installation/repositories", Some(&Pagination::page(page)))
                .await
                .map_err(GitHubError::from)?;
            let last = resp.repositories.len() < Pagination::PER_PAGE as usize;
            all.extend(resp.repositories);
            if last {
                break;
            }
            page += 1;
        }
        Ok(all)
    }
}

/// Outcome of an enqueue attempt, shared by the dashboard API and the PR-comment
/// command so both render the same decision.
pub enum Enqueued {
    /// Added to (or already present in) the queue at this 1-based position.
    Ok {
        entry: QueueEntry,
        position: i32,
        /// The repo has no required checks, so the queue is held (see the engine
        /// guard) — surface a warning to whoever queued it.
        warn_no_checks: bool,
    },
    /// The PR targets a branch other than this repo's queue base; not enqueued.
    WrongBase { pr_base: String, queue_base: String },
}

/// Outcome of a force-dequeue, shared by the dashboard API and the PR-comment
/// command.
pub enum Removed {
    /// The PR was removed (it was queued, or its test batch was cancelled).
    Gone { pr: u64 },
    /// The PR's batch is merging — too late to pull it safely; try again shortly.
    Busy { pr: u64 },
    /// The PR wasn't in the open queue.
    NotQueued,
}

#[derive(Deserialize)]
struct AppInfo {
    owner: AppOwnerRow,
}

#[derive(Deserialize)]
struct AppOwnerRow {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct InstallationRow {
    id: i64,
    account: AccountRow,
}

#[derive(Deserialize)]
struct AccountRow {
    login: String,
}

#[derive(Deserialize)]
struct RepoListResponse {
    repositories: Vec<RepoRow>,
}

#[derive(Deserialize)]
struct RepoRow {
    name: String,
    owner: AccountRow,
    default_branch: String,
}

#[derive(Serialize)]
struct Pagination {
    per_page: u32,
    page: u32,
}

impl Pagination {
    const PER_PAGE: u32 = 100;

    fn page(page: u32) -> Self {
        Self {
            per_page: Self::PER_PAGE,
            page,
        }
    }
}
