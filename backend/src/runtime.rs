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
use crate::queue::{BatchState, Engine, EntryState, LedgerRecord, QueueEntry};
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
    /// for PR-comment queue commands and the dashboard's queue mutations. `false`
    /// when the App isn't configured.
    pub async fn can_write(&self, repo_id: Uuid, username: &str) -> Result<bool> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(false);
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        Ok(client.user_permission(&gh, username).await?.can_write())
    }

    /// The required status-check contexts on `branch`'s protection, read live from
    /// GitHub — used to seed a new queue whose base differs from the repo's default
    /// queue. Empty when the App isn't configured yet.
    pub async fn required_checks(&self, repo_id: Uuid, branch: &str) -> Result<Vec<String>> {
        let Some(client) = self.repo_client.read().await.clone() else {
            return Ok(vec![]);
        };
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        Ok(client.required_checks(&gh, branch).await?)
    }

    /// Test seam: install just the repo client so API authz and branch-protection
    /// reads can be exercised without the full `/setup` credential flow.
    #[cfg(test)]
    pub async fn install_test_repo_client(&self, client: Arc<dyn RepoClient>) {
        *self.repo_client.write().await = Some(client);
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

    /// Validate and enqueue a PR into a specific queue — the single enqueue path for
    /// both the dashboard API and the `/mq queue` PR-comment command. Fetches the PR
    /// live to reject one whose base doesn't match the queue (a backport/release PR
    /// must never be merged into the wrong base), enforces the repo-wide PR-open guard
    /// (a PR is open in at most one queue per repo), then assigns its queue position
    /// under a per-queue advisory lock so concurrent enqueues can't collide on order.
    pub async fn enqueue_pr(&self, queue_id: Uuid, pr: u64, by: &str) -> Result<Enqueued> {
        let cfg = Store::queue_config(&self.db, queue_id).await?;
        let gh = Store::repo_ref(&self.db, cfg.repo_id).await?;
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
        if let Some(existing) = Store::open_entry(&self.db, cfg.repo_id, pr).await?
            && existing.queue_id != queue_id
        {
            let other = Store::queue_config(&self.db, existing.queue_id).await?;
            return Ok(Enqueued::AlreadyQueued { queue: other.name });
        }
        let txn = self.db.begin().await?;
        let bytes = cfg.repo_id.as_bytes();
        let key = i64::from_le_bytes(bytes[..8].try_into().unwrap())
            ^ i64::from_le_bytes(bytes[8..].try_into().unwrap());
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [Value::from(key)],
        ))
        .await?;
        if let Some(existing) = Store::open_entry(&txn, cfg.repo_id, pr).await?
            && existing.queue_id != queue_id
        {
            let other = Store::queue_config(&self.db, existing.queue_id).await?;
            return Ok(Enqueued::AlreadyQueued { queue: other.name });
        }
        let entry = Store::enqueue(&txn, cfg.repo_id, queue_id, pr, &summary.head_sha, by).await?;
        let position = Store::queue_rank(&txn, queue_id, entry.position).await? as i32;
        txn.commit().await?;
        let warn_no_checks = cfg.required_checks.is_empty();
        let mut comment = format!(
            "**mergequeue** · queued · queue `{}` · position {position}",
            cfg.name
        );
        if warn_no_checks {
            comment.push_str(" · ⚠️ no required checks configured (held)");
        }
        let _ = self.comment(cfg.repo_id, pr, &comment).await;
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
    pub async fn force_dequeue(&self, entry_id: Uuid) -> Result<Removed> {
        let Some(entry) = Store::entry(&self.db, entry_id).await? else {
            return Ok(Removed::NotQueued);
        };
        let repo_id = entry.repo_id;
        let queue_id = entry.queue_id;
        let pr = entry.pr_number;
        match entry.state {
            EntryState::Queued => {
                if !Store::dequeue(&self.db, queue_id, entry_id).await? {
                    return Ok(Removed::NotQueued);
                }
            }
            EntryState::Testing => {
                let Some(batch) = Store::active_batch_view(&self.db, queue_id).await? else {
                    return Ok(Removed::NotQueued);
                };
                let entry_ids = batch.entry_ids();
                if !entry_ids.contains(&entry_id) {
                    return Ok(Removed::NotQueued);
                }
                if matches!(batch.state, BatchState::Merging) {
                    let staging_sha = batch.staging_sha.clone().unwrap_or_default();
                    let landed = match self.repo_client.read().await.clone() {
                        Some(client) => {
                            let cfg = Store::queue_config(&self.db, queue_id).await?;
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
                let others: Vec<Uuid> = entry_ids
                    .iter()
                    .copied()
                    .filter(|id| *id != entry_id)
                    .collect();
                if let Some(client) = self.repo_client.read().await.clone() {
                    let gh = Store::repo_ref(&self.db, repo_id).await?;
                    let _ = client.delete_ref(&gh, &batch.staging_ref).await;
                    let _ = client.delete_ref(&gh, &batch.assembly_ref()).await;
                }
                let txn = self.db.begin().await?;
                Store::remove_entry(&txn, queue_id, entry_id).await?;
                Store::requeue_entries(&txn, &others).await?;
                Store::set_batch_state(&txn, batch.id, BatchState::Superseded).await?;
                Store::append_ledger(&txn, &LedgerRecord::removed(&batch, pr)).await?;
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
            EntryState::Merged | EntryState::Ejected => {
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
                let default_queue =
                    Store::get_or_create_queue(&self.db, repo_id, "default").await?;
                Store::set_queue_base_branch(&self.db, default_queue, &repo.default_branch).await?;
                if let Some(client) = &repo_client {
                    let gh = RepoId {
                        owner: repo.owner.login.clone(),
                        name: repo.name.clone(),
                        installation_id: inst.id as u64,
                    };
                    Self::reconcile_required_checks(&self.db, client.as_ref(), &gh, repo_id)
                        .await?;
                }
            }
            // drop repos this installation no longer grants access to.
            Store::prune_installation_repos(&self.db, inst.id, &keep_repos, snapshot).await?;
        }
        // drop installations the App was removed from (cascades their repos).
        Store::prune_installations(&self.db, &keep_installations, snapshot).await?;
        Ok(())
    }

    /// Reconcile EVERY one of a repo's queues' required checks against ITS OWN base
    /// branch's protection — not just the default queue. The default queue tracks the
    /// repo's GitHub default branch (set by the caller); operator-created named queues
    /// keep their own base. Only overwrite on a successful read, so a transient API
    /// error never wipes a queue's checks — an empty set holds the queue (the safety
    /// guard, not a misconfiguration).
    async fn reconcile_required_checks(
        db: &DatabaseConnection,
        client: &dyn RepoClient,
        gh: &RepoId,
        repo_id: Uuid,
    ) -> Result<()> {
        for q in Store::list_queues(db, repo_id).await? {
            match client.required_checks(gh, &q.base_branch).await {
                Ok(checks) => Store::set_queue_required_checks(db, q.id, &checks).await?,
                Err(e) => tracing::warn!(
                    error = %e,
                    repo = %format!("{}/{}", gh.owner, gh.name),
                    branch = %q.base_branch,
                    "could not read branch protection — grant the App administration:read \
                     and re-approve the install; required checks left unchanged (the queue \
                     stays held if it has none yet)"
                ),
            }
        }
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
    /// The PR targets a branch other than this queue's base; not enqueued.
    WrongBase { pr_base: String, queue_base: String },
    /// The PR is already open in another queue of the repo (the repo-wide PR-open
    /// guard); pick that queue or remove it there first.
    AlreadyQueued { queue: String },
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    use async_trait::async_trait;
    use migration::{Migrator, MigratorTrait};
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
    use tokio::sync::Mutex as AsyncMutex;

    use super::Runtime;
    use crate::github::{
        CheckState, GitHubError, MergeOutcome, PullSummary, RepoClient, RepoId, RepoPermission,
    };
    use crate::store::Store;

    /// Serializes DB tests against the shared test database.
    static DB_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    /// A `RepoClient` whose `required_checks` answer is keyed by branch, so a per-queue
    /// reconcile can be asserted to read each queue's OWN base branch.
    struct BranchChecks {
        by_branch: HashMap<String, Vec<String>>,
    }

    #[async_trait]
    impl RepoClient for BranchChecks {
        async fn required_checks(
            &self,
            _: &RepoId,
            branch: &str,
        ) -> Result<Vec<String>, GitHubError> {
            Ok(self.by_branch.get(branch).cloned().unwrap_or_default())
        }
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
                head_sha: String::new(),
                head_ref: String::new(),
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
            Ok(RepoPermission::Write)
        }
    }

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
            "TRUNCATE queue_ledger, batch_entries, batches, queue_entries, queues, repos, \
             installations CASCADE",
        ))
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn test_runtime_reconcile_uses_each_queues_own_base() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        Store::provision_installation(&db, 91, "acme")
            .await
            .unwrap();
        Store::upsert_repo(&db, 91, "acme", "widgets")
            .await
            .unwrap();
        let repo_id = Store::repo_id_by_name(&db, "acme", "widgets")
            .await
            .unwrap()
            .unwrap();
        let default_id = Store::queue_id_by_name(&db, repo_id, "default")
            .await
            .unwrap()
            .unwrap();
        Store::set_queue_base_branch(&db, default_id, "main")
            .await
            .unwrap();
        let rel_id = Store::get_or_create_queue(&db, repo_id, "release")
            .await
            .unwrap();
        Store::set_queue_base_branch(&db, rel_id, "release")
            .await
            .unwrap();

        let fake = BranchChecks {
            by_branch: HashMap::from([
                ("main".to_string(), vec!["ci".to_string()]),
                ("release".to_string(), vec!["release-ci".to_string()]),
            ]),
        };
        let gh = RepoId {
            owner: "acme".into(),
            name: "widgets".into(),
            installation_id: 91,
        };
        Runtime::reconcile_required_checks(&db, &fake, &gh, repo_id)
            .await
            .unwrap();

        assert_eq!(
            Store::queue_config(&db, default_id)
                .await
                .unwrap()
                .required_checks,
            vec!["ci".to_string()],
            "the default queue reconciles against main"
        );
        assert_eq!(
            Store::queue_config(&db, rel_id)
                .await
                .unwrap()
                .required_checks,
            vec!["release-ci".to_string()],
            "a named queue reconciles against ITS OWN base, not the repo default"
        );
    }
}
