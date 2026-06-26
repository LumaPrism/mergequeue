//! The engine driver. Each `tick(repo)` runs a micro-loop: `observe` (read-only
//! IO) → `State::decide` (pure) → `interpret` (write IO), repeating within the
//! tick until the step finishes. The persisted batch row is the resume point:
//! `observe` re-derives the phase from committed state every iteration, and
//! `decide` orders each transition's effects so the state-defining DB write lands
//! before its GitHub side effect — a crash re-dispatches safely.
//!
//! ```text
//! (no batch) --queued?--> Staging --staged--> Testing --green--> Merging --ff--> Merged*
//!                                                 |  \--red--> Bisecting --1 left--> Ejected*
//!                                                 |                       \-->1----> Staging (smaller)
//!                                                 \--base moved--> Superseded*
//! ```
//! (* terminal). Driven by the worker on an interval and on relevant webhooks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use super::EngineError;
use super::model::{BatchState, RepoQueueConfig, TickOutcome};
use super::state::{
    BatchView, DbWrite, Effect, Fact, Flow, GhCall, MergeReport, Observation, State, StepReport,
};
use crate::github::{RepoClient, RepoId};
use crate::store::Store;

pub struct Engine {
    repo: Arc<dyn RepoClient>,
    db: DatabaseConnection,
    /// Per-repo locks serialising concurrent ticks (worker interval + webhooks)
    /// within this process, so two ticks never drive the same batch at once.
    locks: Arc<Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>>,
}

/// A loose bound on micro-loop steps per tick: reset, two per PR (merge-emit and
/// mark), the state hops, and slack. Hitting it means a transition emitted
/// `Continue` without progress — error out rather than spin forever.
fn step_budget(batch_size: usize) -> usize {
    batch_size * 4 + 16
}

impl Engine {
    pub fn new(repo: Arc<dyn RepoClient>, db: DatabaseConnection) -> Self {
        Self {
            repo,
            db,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn repo_lock(&self, repo_id: Uuid) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(repo_id)
            .or_default()
            .clone()
    }

    /// Advance one repo's queue. Serialised per repo; the micro-loop completes a
    /// whole stage/transition in one call (latency parity with the old engine).
    pub async fn tick(&self, repo_id: Uuid) -> Result<TickOutcome, EngineError> {
        let lock = self.repo_lock(repo_id);
        let _guard = lock.lock().await;
        let cfg = Store::repo_config(&self.db, repo_id).await?;
        let mut last: Option<StepReport> = None;
        for _ in 0..step_budget(cfg.batch_size) {
            let obs = self.observe(&cfg, repo_id, last.take()).await?;
            let decision = State::decide(&cfg, &obs);
            last = self.interpret(repo_id, decision.effects).await?;
            if let Flow::Done(outcome) = decision.flow {
                return Ok(outcome);
            }
        }
        Err(EngineError::Other("tick exceeded step budget".into()))
    }

    /// Read-only: gather the facts `decide` needs. The no-checks safety guard
    /// short-circuits before any GitHub IO.
    async fn observe(
        &self,
        cfg: &RepoQueueConfig,
        repo_id: Uuid,
        last: Option<StepReport>,
    ) -> Result<Observation, EngineError> {
        if cfg.required_checks.is_empty() {
            return Ok(Observation::Blocked);
        }
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        let Some(batch) = Store::active_batch_view(&self.db, repo_id).await? else {
            let queued = Store::next_queued(&self.db, repo_id, cfg.batch_size)
                .await?
                .into_iter()
                .map(|e| e.id)
                .collect();
            return Ok(Observation::Empty { queued });
        };
        let fact = self.observe_fact(cfg, &gh, &batch, last).await?;
        Ok(Observation::Active { batch, fact })
    }

    /// The state-specific live fact for an active batch. For Staging the order is
    /// load-bearing: the race guard and the round-tripped merge verdict are both
    /// checked before resolving the next unstaged PR.
    async fn observe_fact(
        &self,
        cfg: &RepoQueueConfig,
        gh: &RepoId,
        batch: &BatchView,
        last: Option<StepReport>,
    ) -> Result<Fact, EngineError> {
        match batch.state {
            BatchState::Staging => {
                if !batch.base_sha.is_empty() {
                    let current = self.repo.base_sha(gh, &cfg.base_branch).await?;
                    if current != batch.base_sha {
                        return Ok(Fact::BaseMoved);
                    }
                }
                if let Some(StepReport::Merged(report)) = last {
                    return Ok(Fact::StageMerged {
                        entry_id: report.entry_id,
                        pr_number: report.pr_number,
                        outcome: report.outcome,
                    });
                }
                if batch.base_sha.is_empty() {
                    let base = self.repo.base_sha(gh, &cfg.base_branch).await?;
                    return Ok(Fact::StageReset { base });
                }
                if let Some(e) = batch.next_unstaged() {
                    let pull = self.repo.pull(gh, e.pr_number).await?;
                    return Ok(Fact::StageNext {
                        entry_id: e.entry_id,
                        pr_number: e.pr_number,
                        head_sha: pull.head_sha,
                        base_ref: pull.base_ref,
                    });
                }
                let staging_sha = self.repo.base_sha(gh, &batch.staging_ref).await?;
                Ok(Fact::StageFinalize { staging_sha })
            }
            BatchState::Testing => {
                let current = self.repo.base_sha(gh, &cfg.base_branch).await?;
                if current != batch.base_sha {
                    return Ok(Fact::BaseMoved);
                }
                let mut reported_per_pr = Vec::with_capacity(batch.entries.len());
                for e in &batch.entries {
                    let head = self.repo.pull(gh, e.pr_number).await?.head_sha;
                    reported_per_pr.push(self.repo.reported_contexts(gh, &head).await?);
                }
                let verdict = match State::applicable_checks(&cfg.required_checks, &reported_per_pr)
                {
                    None => None,
                    Some(set) => {
                        let staging_sha = batch.staging_sha.as_deref().unwrap_or_default();
                        Some(self.repo.check_state(gh, staging_sha, &set).await?)
                    }
                };
                Ok(Fact::Checks { verdict })
            }
            BatchState::Merging => {
                let current = self.repo.base_sha(gh, &cfg.base_branch).await?;
                // A just-rejected fast-forward with base unchanged is a genuine
                // block; if base moved, fall through to the supersede path.
                if matches!(last, Some(StepReport::FfRejected)) && current == batch.base_sha {
                    return Ok(Fact::FfRejected);
                }
                Ok(Fact::MergeBase { current })
            }
            BatchState::Bisecting => Ok(Fact::Bisect),
            BatchState::Merged | BatchState::Ejected | BatchState::Superseded => Err(
                EngineError::Other("active_batch_view returned a terminal batch".into()),
            ),
        }
    }

    /// Write step. Each `Db` group is one transaction; `Gh` calls run singly.
    /// Returns the result-bearing effect's report (if any) for the loop to thread.
    async fn interpret(
        &self,
        repo_id: Uuid,
        effects: Vec<Effect>,
    ) -> Result<Option<StepReport>, EngineError> {
        let gh = Store::repo_ref(&self.db, repo_id).await?;
        let mut report = None;
        for eff in effects {
            match eff {
                Effect::Db(writes) => {
                    let txn = self.db.begin().await?;
                    for w in writes {
                        self.apply_db(&txn, repo_id, w).await?;
                    }
                    txn.commit().await?;
                }
                Effect::Gh(call) => {
                    if let Some(r) = self.apply_gh(&gh, call).await? {
                        report = Some(r);
                    }
                }
            }
        }
        Ok(report)
    }

    async fn apply_db<C: ConnectionTrait>(
        &self,
        c: &C,
        repo_id: Uuid,
        write: DbWrite,
    ) -> Result<(), EngineError> {
        match write {
            DbWrite::CreateBatch {
                entry_ids,
                staging_ref,
            } => Store::create_batch(c, repo_id, &entry_ids, &staging_ref).await?,
            DbWrite::SetBatchBaseSha { batch_id, base_sha } => {
                Store::set_batch_base_sha(c, batch_id, &base_sha).await?
            }
            DbWrite::MarkEntryStaged { batch_id, entry_id } => {
                Store::mark_entry_staged(c, batch_id, entry_id).await?
            }
            DbWrite::SetBatchStaged {
                batch_id,
                staging_sha,
            } => Store::set_batch_staged(c, batch_id, &staging_sha).await?,
            DbWrite::SetBatchState { batch_id, state } => {
                Store::set_batch_state(c, batch_id, state).await?
            }
            DbWrite::SetMergeBlocked { batch_id, blocked } => {
                Store::set_merge_blocked(c, batch_id, blocked).await?
            }
            DbWrite::SetEntriesState { entry_ids, state } => {
                Store::set_entries_state(c, &entry_ids, state).await?
            }
            DbWrite::RequeueEntries { entry_ids } => Store::requeue_entries(c, &entry_ids).await?,
        }
        Ok(())
    }

    /// Run one GitHub effect. `ForceRef`/`MergeOnto`/`FastForward` are fatal on
    /// error (they propagate), except a *rejected* fast-forward, which round-trips
    /// as `FfRejected` instead of wedging the tick. The rest are best-effort.
    async fn apply_gh(&self, gh: &RepoId, call: GhCall) -> Result<Option<StepReport>, EngineError> {
        match call {
            GhCall::ForceRef { staging_ref, sha } => {
                self.repo.force_ref(gh, &staging_ref, &sha).await?;
                Ok(None)
            }
            GhCall::MergeOnto {
                staging_ref,
                head,
                message,
                entry_id,
                pr_number,
            } => {
                let outcome = self
                    .repo
                    .merge_onto(gh, &staging_ref, &head, &message)
                    .await?;
                Ok(Some(StepReport::Merged(MergeReport {
                    entry_id,
                    pr_number,
                    outcome,
                })))
            }
            GhCall::FastForward { base_branch, sha } => {
                match self.repo.fast_forward(gh, &base_branch, &sha).await {
                    Ok(()) => Ok(None),
                    // 422 (not-a-fast-forward / protected) or 403 (ruleset): the
                    // push was rejected, not a transport failure — surface it.
                    Err(e) if matches!(e.status(), Some(422 | 403)) => {
                        Ok(Some(StepReport::FfRejected))
                    }
                    Err(e) => Err(e.into()),
                }
            }
            GhCall::DeleteRef { staging_ref } => {
                let _ = self.repo.delete_ref(gh, &staging_ref).await;
                Ok(None)
            }
            GhCall::Comment { pr, body } => {
                let _ = self.repo.comment(gh, pr, &body).await;
                Ok(None)
            }
            GhCall::SetLabel { pr, target } => {
                let _ = self.repo.set_train_label(gh, pr, target).await;
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use async_trait::async_trait;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    use super::Engine;
    use crate::github::{
        CheckState, GitHubError, MergeOutcome, PullSummary, RepoClient, RepoId, RepoPermission,
        TrainLabel,
    };
    use crate::queue::{BatchState, TickOutcome};
    use crate::store::Store;
    use migration::{Migrator, MigratorTrait};

    /// Serializes DB tests against the shared test database.
    static DB_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    /// In-memory `RepoClient`: tracks branch tips, returns a fixed check verdict,
    /// and records each side effect so transitions can be asserted.
    struct Fake {
        check: CheckState,
        refs: Mutex<HashMap<String, String>>,
        calls: Mutex<Vec<String>>,
        /// base_ref returned by `pull` — defaults to `main`; overridable to test a
        /// PR retargeted off the queue base.
        pull_base: String,
        /// contexts `reported_contexts` returns — defaults to the seeded required
        /// check `ci`; overridable to test path-filtered / not-yet-started checks.
        reported: Vec<String>,
        /// what `merge_onto` returns — defaults to `Merged`; set to `Conflicted` to
        /// test conflict ejection.
        merge: MergeOutcome,
    }

    impl Fake {
        fn new(check: CheckState) -> Self {
            let refs = HashMap::from([("main".to_string(), "base000".to_string())]);
            Self {
                check,
                refs: Mutex::new(refs),
                calls: Mutex::new(vec![]),
                pull_base: "main".to_string(),
                reported: vec!["ci".to_string()],
                merge: MergeOutcome::Merged,
            }
        }

        fn with_pull_base(mut self, base: &str) -> Self {
            self.pull_base = base.to_string();
            self
        }

        fn with_reported(mut self, reported: &[&str]) -> Self {
            self.reported = reported.iter().map(|s| s.to_string()).collect();
            self
        }

        fn with_conflict(mut self) -> Self {
            self.merge = MergeOutcome::Conflicted;
            self
        }

        fn log(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RepoClient for Fake {
        async fn list_open_pulls(&self, _: &RepoId) -> Result<Vec<PullSummary>, GitHubError> {
            Ok(vec![])
        }
        async fn base_sha(&self, _: &RepoId, base: &str) -> Result<String, GitHubError> {
            Ok(self
                .refs
                .lock()
                .unwrap()
                .get(base)
                .cloned()
                .unwrap_or_else(|| "base000".into()))
        }
        async fn pull(&self, _: &RepoId, pr: u64) -> Result<PullSummary, GitHubError> {
            Ok(PullSummary {
                number: pr,
                title: String::new(),
                head_sha: format!("head{pr}"),
                head_ref: format!("feature-{pr}"),
                base_ref: self.pull_base.clone(),
                mergeable: Some(true),
                approved: false,
            })
        }
        async fn merge_onto(
            &self,
            _: &RepoId,
            branch: &str,
            head: &str,
            _: &str,
        ) -> Result<MergeOutcome, GitHubError> {
            self.log(format!("merge_onto {branch} {head}"));
            if self.merge == MergeOutcome::Conflicted {
                return Ok(MergeOutcome::Conflicted);
            }
            let mut refs = self.refs.lock().unwrap();
            let tip = refs.get(branch).cloned().unwrap_or_default();
            if !tip.contains(&format!("+{head}")) {
                refs.insert(branch.to_string(), format!("{tip}+{head}"));
            }
            Ok(MergeOutcome::Merged)
        }
        async fn force_ref(&self, _: &RepoId, branch: &str, sha: &str) -> Result<(), GitHubError> {
            self.log(format!("force_ref {branch} {sha}"));
            self.refs
                .lock()
                .unwrap()
                .insert(branch.to_string(), sha.to_string());
            Ok(())
        }
        async fn delete_ref(&self, _: &RepoId, branch: &str) -> Result<(), GitHubError> {
            self.log(format!("delete_ref {branch}"));
            self.refs.lock().unwrap().remove(branch);
            Ok(())
        }
        async fn check_state(
            &self,
            _: &RepoId,
            _: &str,
            _: &[String],
        ) -> Result<CheckState, GitHubError> {
            Ok(self.check)
        }
        async fn fast_forward(&self, _: &RepoId, base: &str, to: &str) -> Result<(), GitHubError> {
            self.log(format!("fast_forward {base} {to}"));
            self.refs
                .lock()
                .unwrap()
                .insert(base.to_string(), to.to_string());
            Ok(())
        }
        async fn comment(&self, _: &RepoId, pr: u64, _: &str) -> Result<(), GitHubError> {
            self.log(format!("comment {pr}"));
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
        async fn add_labels(
            &self,
            _: &RepoId,
            pr: u64,
            labels: &[String],
        ) -> Result<(), GitHubError> {
            self.log(format!("add_labels {pr} {}", labels.join(",")));
            Ok(())
        }
        async fn remove_label(&self, _: &RepoId, pr: u64, name: &str) -> Result<(), GitHubError> {
            self.log(format!("remove_label {pr} {name}"));
            Ok(())
        }
        async fn user_permission(
            &self,
            _: &RepoId,
            _: &str,
        ) -> Result<RepoPermission, GitHubError> {
            Ok(RepoPermission::Write)
        }
        async fn required_checks(&self, _: &RepoId, _: &str) -> Result<Vec<String>, GitHubError> {
            Ok(vec!["ci".to_string()])
        }
        async fn reported_contexts(&self, _: &RepoId, _: &str) -> Result<Vec<String>, GitHubError> {
            Ok(self.reported.clone())
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
            "TRUNCATE batch_entries, batches, queue_entries, repos, installations CASCADE",
        ))
        .await
        .unwrap();
        db
    }

    async fn seed_repo(db: &DatabaseConnection, batch_size: i32) -> Uuid {
        Store::provision_installation(db, 42, "acme").await.unwrap();
        Store::upsert_repo(db, 42, "acme", "widgets").await.unwrap();
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE repos SET batch_size = {batch_size}, required_checks = '[\"ci\"]'::jsonb \
                 WHERE owner='acme' AND name='widgets'"
            ),
        ))
        .await
        .unwrap();
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id FROM repos WHERE owner='acme' AND name='widgets'",
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get("", "id").unwrap()
    }

    #[tokio::test]
    async fn test_engine_lands_a_green_batch() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 101, "h101", "alice")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Merged { prs: vec![101] });
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        let calls = fake.calls();
        assert!(calls.iter().any(|c| c.starts_with("force_ref")));
        assert!(
            calls
                .iter()
                .any(|c| c == "merge_onto mq/staging/main head101")
        );
        assert!(calls.iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::list_entries(&db, repo_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_engine_ejects_a_red_single() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 202, "h202", "bob")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Failure));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Ejected { pr: 202 });
        assert!(fake.calls().iter().any(|c| c == "comment 202"));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_engine_ignores_a_legacy_badge_status() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 818, "h818", "ivy")
            .await
            .unwrap();

        let fake =
            std::sync::Arc::new(Fake::new(CheckState::Success).with_reported(&["mergequeue"]));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Waiting);
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_engine_relabels_requeued_prs_back_to_queued() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 707, "h707", "gwen")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let batch = Store::active_batch(&db, repo_id).await.unwrap().unwrap();
        Store::set_batch_state(&db, batch.id, BatchState::Merging)
            .await
            .unwrap();
        fake.refs
            .lock()
            .unwrap()
            .insert("main".into(), "moved999".into());

        engine.tick(repo_id).await.unwrap();

        let calls = fake.calls();
        let testing = calls
            .iter()
            .position(|c| c == "add_labels 707 merge-queue: testing");
        let queued = calls
            .iter()
            .position(|c| c == "add_labels 707 merge-queue: queued");
        assert!(
            testing.is_some(),
            "PR should be labelled testing when staged"
        );
        assert!(queued.is_some(), "requeued PR should be relabelled queued");
        assert!(queued > testing, "queued label must come after testing");
    }

    #[tokio::test]
    async fn test_engine_holds_a_pending_batch() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 303, "h303", "carol")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Pending));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Waiting);
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_engine_blocks_a_repo_with_no_required_checks() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE repos SET required_checks = '[]'::jsonb WHERE owner='acme' AND name='widgets'",
        ))
        .await
        .unwrap();
        Store::enqueue(&db, repo_id, 404, "h404", "dave")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success));
        let engine = Engine::new(fake.clone(), db.clone());

        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::BlockedNoChecks);
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        assert!(fake.calls().is_empty());
        assert_eq!(Store::list_entries(&db, repo_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_engine_ejects_a_retargeted_pr_at_staging() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 505, "h505", "erin")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success).with_pull_base("release"));
        let engine = Engine::new(fake.clone(), db.clone());

        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Ejected { pr: 505 });
        assert!(fake.calls().iter().any(|c| c == "comment 505"));
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        assert!(Store::list_entries(&db, repo_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_engine_ejects_a_conflicting_pr() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 111, "h111", "judy")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success).with_conflict());
        let engine = Engine::new(fake.clone(), db.clone());

        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Ejected { pr: 111 });
        assert!(fake.calls().iter().any(|c| c == "comment 111"));
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        assert!(Store::list_entries(&db, repo_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_engine_supersedes_when_base_moves_during_merging() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 606, "h606", "frank")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let batch = Store::active_batch(&db, repo_id).await.unwrap().unwrap();
        Store::set_batch_state(&db, batch.id, BatchState::Merging)
            .await
            .unwrap();
        fake.refs
            .lock()
            .unwrap()
            .insert("main".into(), "moved999".into());

        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Restaged);
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        let open = Store::list_entries(&db, repo_id).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].pr_number, 606);
    }

    #[tokio::test]
    async fn test_engine_merging_resumes_idempotently_after_fast_forward() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 707, "h707", "grace")
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Success));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let batch = Store::active_batch(&db, repo_id).await.unwrap().unwrap();
        let staging_sha = batch.staging_sha.clone().unwrap();
        // Simulate a crash after fast_forward landed: base already == staging tip,
        // batch still persisted as Merging.
        fake.refs.lock().unwrap().insert("main".into(), staging_sha);
        Store::set_batch_state(&db, batch.id, BatchState::Merging)
            .await
            .unwrap();

        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Merged { prs: vec![707] });
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
        assert!(Store::list_entries(&db, repo_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_engine_holds_until_ci_reports() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 808, "h808", "heidi")
            .await
            .unwrap();

        // Nothing has reported on the PR yet → don't merge, hold.
        let fake = std::sync::Arc::new(Fake::new(CheckState::Success).with_reported(&[]));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Waiting);
        assert!(!fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_engine_merges_when_required_check_is_path_filtered() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        Store::enqueue(&db, repo_id, 909, "h909", "ivan")
            .await
            .unwrap();

        // CI ran (lint reported) but the required `ci` check was path-filtered away,
        // so it never reports → it must not block the merge.
        let fake = std::sync::Arc::new(Fake::new(CheckState::Success).with_reported(&["lint"]));
        let engine = Engine::new(fake.clone(), db.clone());

        engine.tick(repo_id).await.unwrap();
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Merged { prs: vec![909] });
        assert!(fake.calls().iter().any(|c| c.starts_with("fast_forward")));
        assert!(Store::active_batch(&db, repo_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_engine_resumes_staging_without_remerging_staged_heads() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 1).await;
        let entry = Store::enqueue(&db, repo_id, 314, "h314", "pi")
            .await
            .unwrap();
        Store::create_batch(&db, repo_id, &[entry.id], "mq/staging/main")
            .await
            .unwrap();
        let batch = Store::active_batch(&db, repo_id).await.unwrap().unwrap();
        Store::set_batch_base_sha(&db, batch.id, "base000")
            .await
            .unwrap();
        Store::mark_entry_staged(&db, batch.id, entry.id)
            .await
            .unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Pending));
        let engine = Engine::new(fake.clone(), db.clone());
        let out = engine.tick(repo_id).await.unwrap();

        assert_eq!(out, TickOutcome::Staged { batch: batch.id });
        let calls = fake.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("force_ref")),
            "resume must not reset the staging branch"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("merge_onto")),
            "resume must not re-merge an already-staged head"
        );
    }

    #[tokio::test]
    async fn test_engine_resume_merges_only_the_unstaged_head() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let repo_id = seed_repo(&db, 2).await;
        let a = Store::enqueue(&db, repo_id, 401, "h401", "ann")
            .await
            .unwrap();
        let b = Store::enqueue(&db, repo_id, 402, "h402", "bo")
            .await
            .unwrap();
        Store::create_batch(&db, repo_id, &[a.id, b.id], "mq/staging/main")
            .await
            .unwrap();
        let batch = Store::active_batch(&db, repo_id).await.unwrap().unwrap();
        Store::set_batch_base_sha(&db, batch.id, "base000")
            .await
            .unwrap();
        Store::mark_entry_staged(&db, batch.id, a.id).await.unwrap();

        let fake = std::sync::Arc::new(Fake::new(CheckState::Pending));
        let engine = Engine::new(fake.clone(), db.clone());
        engine.tick(repo_id).await.unwrap();

        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|c| c == "merge_onto mq/staging/main head402"),
            "the unstaged head must merge"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c == "merge_onto mq/staging/main head401"),
            "the already-staged head must not re-merge"
        );
    }
}
