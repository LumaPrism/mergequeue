//! Persistence. `Store` is a set of connection-generic associated functions over
//! the SeaORM entities in `entity`. Every function takes `&C: ConnectionTrait`, so
//! the *caller* owns the transaction boundary (a `&DatabaseConnection` for a single
//! autocommit statement, or a `&DatabaseTransaction` to make several atomic) — the
//! store never opens one itself. Functions return the storage-agnostic domain types
//! from `queue::model`.

mod entity;

pub use entity::queue_ledger;

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, FixedOffset, Utc};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;

use crate::github::RepoId;
use crate::queue::{
    Batch, BatchState, BatchView, EntryState, EntryView, LedgerRecord, MergeMethod, QueueEntry,
    RepoQueueConfig,
};

/// A repo with its named queues, for the dashboard's switcher.
#[derive(Clone, Debug)]
pub struct RepoSummary {
    pub id: Uuid,
    pub owner: String,
    pub name: String,
    pub queues: Vec<QueueSummary>,
}

/// One queue's config summary plus its live queued depth.
#[derive(Clone, Debug)]
pub struct QueueSummary {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub name: String,
    pub base_branch: String,
    pub batch_size: i32,
    pub queued: i64,
}

const ACTIVE: [BatchState; 4] = [
    BatchState::Staging,
    BatchState::Testing,
    BatchState::Merging,
    BatchState::Bisecting,
];
const OPEN: [EntryState; 2] = [EntryState::Queued, EntryState::Testing];

/// Queue persistence. Zero-sized; all behavior is associated functions.
pub struct Store;

impl Store {
    fn to_entry(m: entity::queue_entry::Model) -> QueueEntry {
        QueueEntry {
            id: m.id,
            repo_id: m.repo_id,
            queue_id: m.queue_id,
            pr_number: m.pr_number as u64,
            position: m.position,
            state: m.state,
            enqueued_by: m.enqueued_by,
            enqueued_at: m.enqueued_at.with_timezone(&Utc),
            head_sha: m.head_sha,
        }
    }

    fn to_batch(m: entity::batch::Model, entry_ids: Vec<Uuid>) -> Batch {
        Batch {
            id: m.id,
            repo_id: m.repo_id,
            queue_id: m.queue_id,
            entry_ids,
            base_sha: m.base_sha,
            staging_sha: m.staging_sha,
            staging_ref: m.staging_ref,
            state: m.state,
            created_at: m.created_at.with_timezone(&Utc),
        }
    }

    async fn batch_entry_ids<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
    ) -> Result<Vec<Uuid>, DbErr> {
        let rows = entity::batch_entry::Entity::find()
            .filter(entity::batch_entry::Column::BatchId.eq(batch_id))
            .order_by_asc(entity::batch_entry::Column::Ord)
            .all(c)
            .await?;
        Ok(rows.into_iter().map(|r| r.entry_id).collect())
    }

    // --- reads ---

    /// Load one queue's config (the engine reads this per tick). Carries the
    /// queue's `repo_id` so callers can resolve GitHub identity from it.
    pub async fn queue_config<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
    ) -> Result<RepoQueueConfig, DbErr> {
        let m = entity::queue::Entity::find_by_id(queue_id)
            .one(c)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("queue {queue_id}")))?;
        Ok(RepoQueueConfig {
            queue_id: m.id,
            repo_id: m.repo_id,
            name: m.name,
            base_branch: m.base_branch,
            batch_size: m.batch_size as usize,
            required_checks: m.required_checks.0,
            merge_method: m.merge_method,
            staging_prefix: m.staging_prefix,
        })
    }

    pub async fn repo_ref<C: ConnectionTrait>(c: &C, repo_id: Uuid) -> Result<RepoId, DbErr> {
        let repo = entity::repo::Entity::find_by_id(repo_id)
            .one(c)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("repo {repo_id}")))?;
        let inst = entity::installation::Entity::find_by_id(repo.installation_pk)
            .one(c)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("installation for repo {repo_id}")))?;
        Ok(RepoId {
            owner: repo.owner,
            name: repo.name,
            installation_id: inst.installation_id as u64,
        })
    }

    pub async fn active_batch<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
    ) -> Result<Option<Batch>, DbErr> {
        let Some(m) = entity::batch::Entity::find()
            .filter(entity::batch::Column::QueueId.eq(queue_id))
            .filter(entity::batch::Column::State.is_in(ACTIVE))
            .one(c)
            .await?
        else {
            return Ok(None);
        };
        let entry_ids = Self::batch_entry_ids(c, m.id).await?;
        Ok(Some(Self::to_batch(m, entry_ids)))
    }

    /// The queue's active batch projected for the FSM: entries in `ord` order, each
    /// with its PR number and per-PR `staged` progress. `None` if no batch is active.
    pub async fn active_batch_view<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
    ) -> Result<Option<BatchView>, DbErr> {
        let Some(m) = entity::batch::Entity::find()
            .filter(entity::batch::Column::QueueId.eq(queue_id))
            .filter(entity::batch::Column::State.is_in(ACTIVE))
            .one(c)
            .await?
        else {
            return Ok(None);
        };
        let rows = entity::batch_entry::Entity::find()
            .filter(entity::batch_entry::Column::BatchId.eq(m.id))
            .order_by_asc(entity::batch_entry::Column::Ord)
            .all(c)
            .await?;
        let pr_by_id: HashMap<Uuid, u64> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::Id.is_in(rows.iter().map(|r| r.entry_id)))
            .all(c)
            .await?
            .into_iter()
            .map(|q| (q.id, q.pr_number as u64))
            .collect();
        let entries = rows
            .into_iter()
            .map(|r| EntryView {
                pr_number: pr_by_id.get(&r.entry_id).copied().unwrap_or_default(),
                entry_id: r.entry_id,
                staged: r.staged,
            })
            .collect();
        Ok(Some(BatchView {
            id: m.id,
            repo_id: m.repo_id,
            queue_id: m.queue_id,
            state: m.state,
            base_sha: m.base_sha,
            staging_sha: m.staging_sha,
            staging_ref: m.staging_ref,
            merge_blocked: m.merge_blocked,
            entries,
            created_at: m.created_at.with_timezone(&Utc),
        }))
    }

    pub async fn next_queued<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        n: usize,
    ) -> Result<Vec<QueueEntry>, DbErr> {
        let rows = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .order_by_asc(entity::queue_entry::Column::Position)
            .limit(n as u64)
            .all(c)
            .await?;
        Ok(rows.into_iter().map(Self::to_entry).collect())
    }

    pub async fn entries_by_ids<C: ConnectionTrait>(
        c: &C,
        ids: &[Uuid],
    ) -> Result<Vec<QueueEntry>, DbErr> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::Id.is_in(ids.iter().copied()))
            .all(c)
            .await?;
        let mut by_id: HashMap<Uuid, QueueEntry> = rows
            .into_iter()
            .map(|m| {
                let e = Self::to_entry(m);
                (e.id, e)
            })
            .collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    pub async fn entry_prs<C: ConnectionTrait>(c: &C, ids: &[Uuid]) -> Result<Vec<u64>, DbErr> {
        Ok(Self::entries_by_ids(c, ids)
            .await?
            .into_iter()
            .map(|e| e.pr_number)
            .collect())
    }

    // --- FSM transitions ---

    pub async fn create_batch<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        queue_id: Uuid,
        entry_ids: &[Uuid],
        staging_ref: &str,
    ) -> Result<(), DbErr> {
        let id = Uuid::new_v4();
        entity::batch::ActiveModel {
            id: Set(id),
            repo_id: Set(repo_id),
            queue_id: Set(queue_id),
            base_sha: Set(String::new()),
            staging_ref: Set(staging_ref.to_owned()),
            state: Set(BatchState::Staging),
            ..Default::default()
        }
        .insert(c)
        .await?;
        for (ord, entry_id) in entry_ids.iter().enumerate() {
            entity::batch_entry::ActiveModel {
                batch_id: Set(id),
                entry_id: Set(*entry_id),
                ord: Set(ord as i32),
                staged: Set(false),
            }
            .insert(c)
            .await?;
        }
        Ok(())
    }

    /// Record the base tip a staging build was reset onto (the race anchor).
    pub async fn set_batch_base_sha<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
        base_sha: &str,
    ) -> Result<(), DbErr> {
        entity::batch::ActiveModel {
            id: Set(batch_id),
            base_sha: Set(base_sha.to_owned()),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    /// Mark a PR's head as durably merged onto the staging branch.
    pub async fn mark_entry_staged<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
        entry_id: Uuid,
    ) -> Result<(), DbErr> {
        entity::batch_entry::ActiveModel {
            batch_id: Set(batch_id),
            entry_id: Set(entry_id),
            staged: Set(true),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    /// Mark (or clear) a batch as merge-blocked — its base fast-forward was
    /// rejected. Drives the once-only "blocked" comment/label.
    pub async fn set_merge_blocked<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
        blocked: bool,
    ) -> Result<(), DbErr> {
        entity::batch::ActiveModel {
            id: Set(batch_id),
            merge_blocked: Set(blocked),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    /// Record the assembled staging tip and flip the batch to `Testing`. The base
    /// tip was already persisted at the reset step (`set_batch_base_sha`).
    pub async fn set_batch_staged<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
        staging_sha: &str,
    ) -> Result<(), DbErr> {
        entity::batch::ActiveModel {
            id: Set(batch_id),
            staging_sha: Set(Some(staging_sha.to_owned())),
            state: Set(BatchState::Testing),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    pub async fn set_batch_state<C: ConnectionTrait>(
        c: &C,
        batch_id: Uuid,
        state: BatchState,
    ) -> Result<(), DbErr> {
        entity::batch::ActiveModel {
            id: Set(batch_id),
            state: Set(state),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    pub async fn set_entries_state<C: ConnectionTrait>(
        c: &C,
        ids: &[Uuid],
        state: EntryState,
    ) -> Result<(), DbErr> {
        for &id in ids {
            entity::queue_entry::ActiveModel {
                id: Set(id),
                state: Set(state),
                ..Default::default()
            }
            .update(c)
            .await?;
        }
        Ok(())
    }

    pub async fn requeue_entries<C: ConnectionTrait>(c: &C, ids: &[Uuid]) -> Result<(), DbErr> {
        Self::set_entries_state(c, ids, EntryState::Queued).await
    }

    /// Append a finished batch's terminal outcome to the append-only ledger. The
    /// FSM stays pure, so `ended_at` is stamped here at the IO boundary. `batch_id`
    /// is UNIQUE and the engine replays effects on crash-resume, so a duplicate
    /// append is a no-op (`ON CONFLICT DO NOTHING`).
    pub async fn append_ledger<C: ConnectionTrait>(
        c: &C,
        record: &LedgerRecord,
    ) -> Result<(), DbErr> {
        let entries = serde_json::to_value(&record.entries)
            .map_err(|e| DbErr::Custom(format!("serialize ledger entries: {e}")))?;
        let res = entity::queue_ledger::Entity::insert(entity::queue_ledger::ActiveModel {
            id: Set(Uuid::new_v4()),
            repo_id: Set(record.repo_id),
            queue_id: Set(record.queue_id),
            batch_id: Set(record.batch_id),
            outcome: Set(record.outcome),
            base_sha: Set(record.base_sha.clone()),
            landed_sha: Set(record.landed_sha.clone()),
            ejected_pr: Set(record.ejected_pr.map(|p| p as i64)),
            entries: Set(entries),
            started_at: Set(record.started_at.into()),
            ended_at: Set(Utc::now().into()),
        })
        .on_conflict(
            OnConflict::column(entity::queue_ledger::Column::BatchId)
                .do_nothing()
                .to_owned(),
        )
        .exec(c)
        .await;
        match res {
            Ok(_) | Err(DbErr::RecordNotInserted) => Ok(()),
            Err(e) => Err(e),
        }
    }

    // --- api / ui ---

    /// The queue's most recent finished batch runs, newest first.
    pub async fn list_ledger<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        limit: u64,
    ) -> Result<Vec<entity::queue_ledger::Model>, DbErr> {
        entity::queue_ledger::Entity::find()
            .filter(entity::queue_ledger::Column::QueueId.eq(queue_id))
            .order_by_desc(entity::queue_ledger::Column::EndedAt)
            .limit(limit)
            .all(c)
            .await
    }

    pub async fn list_entries<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
    ) -> Result<Vec<QueueEntry>, DbErr> {
        let rows = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .order_by_asc(entity::queue_entry::Column::Position)
            .all(c)
            .await?;
        Ok(rows.into_iter().map(Self::to_entry).collect())
    }

    /// 1-based rank of `position` among the queue's OPEN entries. Counts entries
    /// strictly ahead, so a reported queue position stays honest even when dequeues
    /// have left gaps in the raw `position` sequence.
    pub async fn queue_rank<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        position: i32,
    ) -> Result<i64, DbErr> {
        let ahead = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .filter(entity::queue_entry::Column::Position.lt(position))
            .count(c)
            .await?;
        Ok(ahead as i64 + 1)
    }

    /// The full queue entry behind an id, or `None` if it's gone. Carries its
    /// `queue_id`/`repo_id` so a PR-keyed mutation (force-dequeue) can resolve the
    /// queue it belongs to.
    pub async fn entry<C: ConnectionTrait>(
        c: &C,
        entry_id: Uuid,
    ) -> Result<Option<QueueEntry>, DbErr> {
        Ok(entity::queue_entry::Entity::find_by_id(entry_id)
            .one(c)
            .await?
            .map(Self::to_entry))
    }

    /// A PR's open entry anywhere in the repo (`Queued` or `Testing`), or `None`.
    /// The PR-open guard is repo-wide — a PR is open in at most one queue per repo —
    /// so this is the single open entry for the PR if it exists.
    pub async fn open_entry<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
    ) -> Result<Option<QueueEntry>, DbErr> {
        Ok(entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number as i64))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .one(c)
            .await?
            .map(Self::to_entry))
    }

    /// The open-queue state of a PR (`Queued` or `Testing`), or `None` if it isn't
    /// in the queue. Lets a dequeue command report accurately instead of blindly
    /// confirming a removal that didn't happen.
    pub async fn open_entry_state<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
    ) -> Result<Option<EntryState>, DbErr> {
        Ok(Self::open_entry(c, repo_id, pr_number)
            .await?
            .map(|e| e.state))
    }

    /// The id of a PR's open queue entry (`Queued` or `Testing`), if any — lets a
    /// PR-keyed caller (the comment command) drive force-dequeue.
    pub async fn open_entry_id<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
    ) -> Result<Option<Uuid>, DbErr> {
        Ok(Self::open_entry(c, repo_id, pr_number).await?.map(|e| e.id))
    }

    pub async fn list_repos<C: ConnectionTrait>(c: &C) -> Result<Vec<RepoSummary>, DbErr> {
        let repos = entity::repo::Entity::find()
            .order_by_asc(entity::repo::Column::Owner)
            .order_by_asc(entity::repo::Column::Name)
            .all(c)
            .await?;
        let queues = entity::queue::Entity::find()
            .order_by_asc(entity::queue::Column::Name)
            .all(c)
            .await?;
        let counts: Vec<(Uuid, i64)> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .select_only()
            .column(entity::queue_entry::Column::QueueId)
            .column_as(entity::queue_entry::Column::Id.count(), "n")
            .group_by(entity::queue_entry::Column::QueueId)
            .into_tuple()
            .all(c)
            .await?;
        let depth: HashMap<Uuid, i64> = counts.into_iter().collect();
        let mut by_repo: HashMap<Uuid, Vec<QueueSummary>> = HashMap::new();
        for q in queues {
            by_repo.entry(q.repo_id).or_default().push(QueueSummary {
                queued: depth.get(&q.id).copied().unwrap_or(0),
                id: q.id,
                repo_id: q.repo_id,
                name: q.name,
                base_branch: q.base_branch,
                batch_size: q.batch_size,
            });
        }
        Ok(repos
            .into_iter()
            .map(|r| RepoSummary {
                queues: by_repo.remove(&r.id).unwrap_or_default(),
                id: r.id,
                owner: r.owner,
                name: r.name,
            })
            .collect())
    }

    /// The queues a repo hosts, each with its live queued depth (the per-repo queue
    /// switcher / list endpoint).
    pub async fn list_queues<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
    ) -> Result<Vec<QueueSummary>, DbErr> {
        let queues = entity::queue::Entity::find()
            .filter(entity::queue::Column::RepoId.eq(repo_id))
            .order_by_asc(entity::queue::Column::Name)
            .all(c)
            .await?;
        let counts: Vec<(Uuid, i64)> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .select_only()
            .column(entity::queue_entry::Column::QueueId)
            .column_as(entity::queue_entry::Column::Id.count(), "n")
            .group_by(entity::queue_entry::Column::QueueId)
            .into_tuple()
            .all(c)
            .await?;
        let depth: HashMap<Uuid, i64> = counts.into_iter().collect();
        Ok(queues
            .into_iter()
            .map(|q| QueueSummary {
                queued: depth.get(&q.id).copied().unwrap_or(0),
                id: q.id,
                repo_id: q.repo_id,
                name: q.name,
                base_branch: q.base_branch,
                batch_size: q.batch_size,
            })
            .collect())
    }

    /// Queue ids (paired with their repo) that have work — an open entry or an
    /// active batch — so the worker ticks only queues that need advancing.
    pub async fn active_queue_ids<C: ConnectionTrait>(c: &C) -> Result<Vec<(Uuid, Uuid)>, DbErr> {
        let mut ids: BTreeSet<Uuid> = BTreeSet::new();
        let open: Vec<Uuid> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .select_only()
            .column(entity::queue_entry::Column::QueueId)
            .group_by(entity::queue_entry::Column::QueueId)
            .into_tuple()
            .all(c)
            .await?;
        ids.extend(open);
        let active: Vec<Uuid> = entity::batch::Entity::find()
            .filter(entity::batch::Column::State.is_in(ACTIVE))
            .select_only()
            .column(entity::batch::Column::QueueId)
            .group_by(entity::batch::Column::QueueId)
            .into_tuple()
            .all(c)
            .await?;
        ids.extend(active);
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let queues = entity::queue::Entity::find()
            .filter(entity::queue::Column::Id.is_in(ids.iter().copied()))
            .all(c)
            .await?;
        Ok(queues.into_iter().map(|q| (q.id, q.repo_id)).collect())
    }

    pub async fn enqueue<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        queue_id: Uuid,
        pr_number: u64,
        head_sha: &str,
        by: &str,
    ) -> Result<QueueEntry, DbErr> {
        if let Some(m) = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number as i64))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .one(c)
            .await?
        {
            return Ok(Self::to_entry(m));
        }
        let max_pos: Option<i32> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .select_only()
            .column_as(entity::queue_entry::Column::Position.max(), "m")
            .into_tuple::<Option<i32>>()
            .one(c)
            .await?
            .flatten();
        let m = entity::queue_entry::ActiveModel {
            id: Set(Uuid::new_v4()),
            repo_id: Set(repo_id),
            queue_id: Set(queue_id),
            pr_number: Set(pr_number as i64),
            position: Set(max_pos.map_or(0, |p| p + 1)),
            state: Set(EntryState::Queued),
            enqueued_by: Set(by.to_owned()),
            head_sha: Set(head_sha.to_owned()),
            ..Default::default()
        }
        .insert(c)
        .await?;
        Ok(Self::to_entry(m))
    }

    /// Remove a queued entry from a queue. Returns whether a row was actually
    /// deleted (only `Queued` entries in this queue are removable), so the caller
    /// only announces a removal that really happened.
    pub async fn dequeue<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        entry_id: Uuid,
    ) -> Result<bool, DbErr> {
        let res = entity::queue_entry::Entity::delete_many()
            .filter(entity::queue_entry::Column::Id.eq(entry_id))
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .exec(c)
            .await?;
        Ok(res.rows_affected > 0)
    }

    /// Hard-delete an entry whatever its state — used by force-dequeue to pull a
    /// PR that's already testing in the active batch. Returns whether a row was
    /// removed.
    pub async fn remove_entry<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        entry_id: Uuid,
    ) -> Result<bool, DbErr> {
        let res = entity::queue_entry::Entity::delete_many()
            .filter(entity::queue_entry::Column::Id.eq(entry_id))
            .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
            .exec(c)
            .await?;
        Ok(res.rows_affected > 0)
    }

    pub async fn reorder<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        ordered: &[Uuid],
    ) -> Result<(), DbErr> {
        for (pos, &id) in ordered.iter().enumerate() {
            entity::queue_entry::Entity::update_many()
                .col_expr(
                    entity::queue_entry::Column::Position,
                    Expr::value(pos as i32),
                )
                .filter(entity::queue_entry::Column::Id.eq(id))
                .filter(entity::queue_entry::Column::QueueId.eq(queue_id))
                .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
                .exec(c)
                .await?;
        }
        Ok(())
    }

    // --- provisioning (webhook) ---

    pub async fn provision_installation<C: ConnectionTrait>(
        c: &C,
        installation_id: i64,
        account_login: &str,
    ) -> Result<(), DbErr> {
        entity::installation::Entity::insert(entity::installation::ActiveModel {
            id: Set(Uuid::new_v4()),
            installation_id: Set(installation_id),
            account_login: Set(account_login.to_owned()),
            status: Set("active".to_owned()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(entity::installation::Column::InstallationId)
                .update_columns([
                    entity::installation::Column::AccountLogin,
                    entity::installation::Column::Status,
                ])
                .to_owned(),
        )
        .exec(c)
        .await?;
        Ok(())
    }

    pub async fn deprovision_installation<C: ConnectionTrait>(
        c: &C,
        installation_id: i64,
    ) -> Result<(), DbErr> {
        entity::installation::Entity::delete_many()
            .filter(entity::installation::Column::InstallationId.eq(installation_id))
            .exec(c)
            .await?;
        Ok(())
    }

    /// Reconcile: drop installations whose `installation_id` is not in `keep`
    /// (cascades their repos). Only rows created at/before `before` are eligible, so
    /// an installation a webhook provisions *during* the sync (after the fetch
    /// snapshot) is never deleted. An empty `keep` prunes every predating row.
    pub async fn prune_installations<C: ConnectionTrait>(
        c: &C,
        keep: &[i64],
        before: DateTime<FixedOffset>,
    ) -> Result<(), DbErr> {
        let mut q = entity::installation::Entity::delete_many()
            .filter(entity::installation::Column::CreatedAt.lt(before));
        if !keep.is_empty() {
            q = q.filter(
                entity::installation::Column::InstallationId.is_not_in(keep.iter().copied()),
            );
        }
        q.exec(c).await?;
        Ok(())
    }

    /// Reconcile: within one installation, drop repos whose `name` is not in `keep`
    /// (the installation grants access to a single account, so name is unique here).
    /// Bounded by `before` so a repo a concurrent webhook adds isn't deleted.
    pub async fn prune_installation_repos<C: ConnectionTrait>(
        c: &C,
        installation_id: i64,
        keep: &[String],
        before: DateTime<FixedOffset>,
    ) -> Result<(), DbErr> {
        let Some(inst) = entity::installation::Entity::find()
            .filter(entity::installation::Column::InstallationId.eq(installation_id))
            .one(c)
            .await?
        else {
            return Ok(());
        };
        let mut q = entity::repo::Entity::delete_many()
            .filter(entity::repo::Column::InstallationPk.eq(inst.id))
            .filter(entity::repo::Column::CreatedAt.lt(before));
        if !keep.is_empty() {
            q = q.filter(entity::repo::Column::Name.is_not_in(keep.iter().cloned()));
        }
        q.exec(c).await?;
        Ok(())
    }

    pub async fn upsert_repo<C: ConnectionTrait>(
        c: &C,
        installation_id: i64,
        owner: &str,
        name: &str,
    ) -> Result<(), DbErr> {
        let Some(inst) = entity::installation::Entity::find()
            .filter(entity::installation::Column::InstallationId.eq(installation_id))
            .one(c)
            .await?
        else {
            return Ok(());
        };
        entity::repo::Entity::insert(entity::repo::ActiveModel {
            id: Set(Uuid::new_v4()),
            installation_pk: Set(inst.id),
            owner: Set(owner.to_owned()),
            name: Set(name.to_owned()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::columns([entity::repo::Column::Owner, entity::repo::Column::Name])
                .update_column(entity::repo::Column::InstallationPk)
                .to_owned(),
        )
        .exec(c)
        .await?;
        if let Some(repo_id) = Self::repo_id_by_name(c, owner, name).await? {
            Self::get_or_create_queue(c, repo_id, "default").await?;
        }
        Ok(())
    }

    /// Create a named queue with explicit config. Idempotent on `(repo_id, name)`:
    /// a conflict leaves the existing queue untouched. Returns the queue id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_queue<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        name: &str,
        base_branch: &str,
        batch_size: i32,
        merge_method: MergeMethod,
        staging_prefix: &str,
        required_checks: &[String],
    ) -> Result<Uuid, DbErr> {
        entity::queue::Entity::insert(entity::queue::ActiveModel {
            id: Set(Uuid::new_v4()),
            repo_id: Set(repo_id),
            name: Set(name.to_owned()),
            base_branch: Set(base_branch.to_owned()),
            batch_size: Set(batch_size),
            merge_method: Set(merge_method),
            staging_prefix: Set(staging_prefix.to_owned()),
            required_checks: Set(entity::RequiredChecks(required_checks.to_vec())),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::columns([entity::queue::Column::RepoId, entity::queue::Column::Name])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(c)
        .await?;
        Self::queue_id_by_name(c, repo_id, name)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("queue {repo_id}/{name}")))
    }

    /// Resolve a queue's id by `(repo_id, name)`, or create it if missing. A new
    /// non-default queue clones the repo's `default` queue config (so it inherits the
    /// repo's base branch + required checks); the `default` queue itself falls back to
    /// the table defaults, which `sync_installations` then reconciles from GitHub.
    pub async fn get_or_create_queue<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        name: &str,
    ) -> Result<Uuid, DbErr> {
        if let Some(id) = Self::queue_id_by_name(c, repo_id, name).await? {
            return Ok(id);
        }
        let default = if name == "default" {
            None
        } else {
            entity::queue::Entity::find()
                .filter(entity::queue::Column::RepoId.eq(repo_id))
                .filter(entity::queue::Column::Name.eq("default"))
                .one(c)
                .await?
        };
        match default {
            Some(d) => {
                Self::create_queue(
                    c,
                    repo_id,
                    name,
                    &d.base_branch,
                    d.batch_size,
                    d.merge_method,
                    &d.staging_prefix,
                    &d.required_checks.0,
                )
                .await
            }
            None => {
                Self::create_queue(
                    c,
                    repo_id,
                    name,
                    "main",
                    1,
                    MergeMethod::Squash,
                    "mq/staging",
                    &[],
                )
                .await
            }
        }
    }

    /// A queue's id by `(repo_id, name)`, or `None` if no such queue.
    pub async fn queue_id_by_name<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        name: &str,
    ) -> Result<Option<Uuid>, DbErr> {
        Ok(entity::queue::Entity::find()
            .filter(entity::queue::Column::RepoId.eq(repo_id))
            .filter(entity::queue::Column::Name.eq(name))
            .one(c)
            .await?
            .map(|q| q.id))
    }

    /// The repo a queue belongs to, or `None` if the queue is gone.
    pub async fn queue_repo_id<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
    ) -> Result<Option<Uuid>, DbErr> {
        Ok(entity::queue::Entity::find_by_id(queue_id)
            .one(c)
            .await?
            .map(|q| q.repo_id))
    }

    /// The internal id of a managed repo by `owner/name`, or `None` if unmanaged.
    pub async fn repo_id_by_name<C: ConnectionTrait>(
        c: &C,
        owner: &str,
        name: &str,
    ) -> Result<Option<Uuid>, DbErr> {
        Ok(entity::repo::Entity::find()
            .filter(entity::repo::Column::Owner.eq(owner))
            .filter(entity::repo::Column::Name.eq(name))
            .one(c)
            .await?
            .map(|r| r.id))
    }

    /// Set a queue's base branch (the default queue is synced from the repo's GitHub
    /// default branch; operator-created queues are left alone by the sync).
    pub async fn set_queue_base_branch<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        branch: &str,
    ) -> Result<(), DbErr> {
        entity::queue::ActiveModel {
            id: Set(queue_id),
            base_branch: Set(branch.to_owned()),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    /// Set a queue's required check contexts (the default queue is synced from GitHub
    /// branch protection). These are the contexts the engine gates a batch on; empty
    /// holds the queue.
    pub async fn set_queue_required_checks<C: ConnectionTrait>(
        c: &C,
        queue_id: Uuid,
        checks: &[String],
    ) -> Result<(), DbErr> {
        entity::queue::ActiveModel {
            id: Set(queue_id),
            required_checks: Set(entity::RequiredChecks(checks.to_vec())),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    pub async fn delete_repo<C: ConnectionTrait>(
        c: &C,
        owner: &str,
        name: &str,
    ) -> Result<(), DbErr> {
        entity::repo::Entity::delete_many()
            .filter(entity::repo::Column::Owner.eq(owner))
            .filter(entity::repo::Column::Name.eq(name))
            .exec(c)
            .await?;
        Ok(())
    }

    /// Drop a PR's queued entry by `owner/name` + number. Returns whether a row was
    /// actually removed (so callers only clear the PR's badge when it really left).
    pub async fn dequeue_pr<C: ConnectionTrait>(
        c: &C,
        owner: &str,
        name: &str,
        pr_number: i64,
    ) -> Result<bool, DbErr> {
        let Some(repo) = entity::repo::Entity::find()
            .filter(entity::repo::Column::Owner.eq(owner))
            .filter(entity::repo::Column::Name.eq(name))
            .one(c)
            .await?
        else {
            return Ok(false);
        };
        let res = entity::queue_entry::Entity::delete_many()
            .filter(entity::queue_entry::Column::RepoId.eq(repo.id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number))
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .exec(c)
            .await?;
        Ok(res.rows_affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use chrono::{Duration, Utc};
    use migration::{Migrator, MigratorTrait};
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Set,
        Statement,
    };
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    use super::{Store, entity};
    use crate::queue::{LedgerEntry, LedgerEntryResult, LedgerOutcome, LedgerRecord};

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
            "TRUNCATE queue_ledger, batch_entries, batches, queue_entries, queues, repos, \
             installations CASCADE",
        ))
        .await
        .unwrap();
        db
    }

    /// Seed a repo and return `(repo_id, default_queue_id)` — `upsert_repo` creates
    /// the `default` queue.
    async fn seed_repo(db: &DatabaseConnection) -> (Uuid, Uuid) {
        Store::provision_installation(db, 77, "acme").await.unwrap();
        Store::upsert_repo(db, 77, "acme", "widgets").await.unwrap();
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

    fn record(
        repo_id: Uuid,
        queue_id: Uuid,
        batch_id: Uuid,
        outcome: LedgerOutcome,
    ) -> LedgerRecord {
        LedgerRecord {
            batch_id,
            repo_id,
            queue_id,
            outcome,
            base_sha: "base000".into(),
            landed_sha: Some("stg777".into()),
            ejected_pr: None,
            entries: vec![
                LedgerEntry {
                    pr_number: 101,
                    result: LedgerEntryResult::Landed,
                },
                LedgerEntry {
                    pr_number: 102,
                    result: LedgerEntryResult::Requeued,
                },
            ],
            started_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_store_ledger_round_trips_one_record() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let rec = record(repo_id, queue_id, Uuid::new_v4(), LedgerOutcome::Merged);
        Store::append_ledger(&db, &rec).await.unwrap();

        let rows = Store::list_ledger(&db, queue_id, 50).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.batch_id, rec.batch_id);
        assert_eq!(row.outcome, LedgerOutcome::Merged);
        assert_eq!(row.landed_sha.as_deref(), Some("stg777"));
        assert!(row.ejected_pr.is_none());
        let entries: Vec<LedgerEntry> = serde_json::from_value(row.entries.clone()).unwrap();
        assert_eq!(entries, rec.entries);
    }

    #[tokio::test]
    async fn test_store_ledger_append_twice_is_idempotent() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let rec = record(repo_id, queue_id, Uuid::new_v4(), LedgerOutcome::Ejected);
        Store::append_ledger(&db, &rec).await.unwrap();
        Store::append_ledger(&db, &rec).await.unwrap();
        let rows = Store::list_ledger(&db, queue_id, 50).await.unwrap();
        assert_eq!(rows.len(), 1, "a duplicate batch_id append must be a no-op");
    }

    #[tokio::test]
    async fn test_store_ledger_list_orders_newest_first() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let base = Utc::now();
        let mut ids = Vec::new();
        for i in 0..3i64 {
            let id = Uuid::new_v4();
            ids.push(id);
            entity::queue_ledger::ActiveModel {
                id: Set(id),
                repo_id: Set(repo_id),
                queue_id: Set(queue_id),
                batch_id: Set(Uuid::new_v4()),
                outcome: Set(LedgerOutcome::Merged),
                base_sha: Set("base000".into()),
                landed_sha: Set(None),
                ejected_pr: Set(None),
                entries: Set(serde_json::json!([])),
                started_at: Set(base.into()),
                ended_at: Set((base + Duration::seconds(i)).into()),
            }
            .insert(&db)
            .await
            .unwrap();
        }
        let rows = Store::list_ledger(&db, queue_id, 2).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, ids[2], "newest row comes first");
        assert_eq!(rows[1].id, ids[1]);
    }

    #[tokio::test]
    async fn test_store_get_or_create_queue_is_idempotent() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let again = Store::get_or_create_queue(&db, repo_id, "default")
            .await
            .unwrap();
        assert_eq!(
            again, default_id,
            "default queue resolves, never duplicates"
        );
        let fe1 = Store::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();
        let fe2 = Store::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();
        assert_eq!(fe1, fe2, "a named queue is created once, then resolved");
        assert_ne!(fe1, default_id);
        let queues = Store::list_queues(&db, repo_id).await.unwrap();
        assert_eq!(queues.len(), 2, "default + frontend");
    }

    #[tokio::test]
    async fn test_store_active_batch_is_isolated_per_queue() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let fe_id = Store::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();

        let a = Store::enqueue(&db, repo_id, default_id, 101, "h101", "alice")
            .await
            .unwrap();
        let b = Store::enqueue(&db, repo_id, fe_id, 202, "h202", "bob")
            .await
            .unwrap();
        Store::create_batch(&db, repo_id, default_id, &[a.id], "mq/staging/default/main")
            .await
            .unwrap();
        Store::create_batch(&db, repo_id, fe_id, &[b.id], "mq/staging/frontend/main")
            .await
            .unwrap();

        let da = Store::active_batch_view(&db, default_id)
            .await
            .unwrap()
            .expect("default queue has an active batch");
        let fb = Store::active_batch_view(&db, fe_id)
            .await
            .unwrap()
            .expect("frontend queue has an active batch");
        assert_ne!(
            da.id, fb.id,
            "each queue in the repo holds its own active batch concurrently"
        );
        assert_eq!(da.queue_id, default_id);
        assert_eq!(fb.queue_id, fe_id);
        assert_eq!(da.prs(), vec![101]);
        assert_eq!(fb.prs(), vec![202]);
    }

    #[tokio::test]
    async fn test_store_enqueue_pr_open_guard_is_repo_wide() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let fe_id = Store::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();

        Store::enqueue(&db, repo_id, default_id, 303, "h303", "carol")
            .await
            .unwrap();
        let dup = Store::enqueue(&db, repo_id, fe_id, 303, "h303", "carol").await;
        assert!(
            dup.is_err(),
            "the repo-wide PR-open guard rejects the same PR open in a second queue"
        );
        assert_eq!(Store::list_entries(&db, default_id).await.unwrap().len(), 1);
        assert!(Store::list_entries(&db, fe_id).await.unwrap().is_empty());
    }
}
