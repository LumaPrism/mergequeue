//! Persistence. `Store` is a set of connection-generic associated functions over
//! the SeaORM entities in `entity`. Every function takes `&C: ConnectionTrait`, so
//! the *caller* owns the transaction boundary (a `&DatabaseConnection` for a single
//! autocommit statement, or a `&DatabaseTransaction` to make several atomic) — the
//! store never opens one itself. Functions return the storage-agnostic domain types
//! from `queue::model`.

mod entity;

use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Utc};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;

use crate::github::RepoId;
use crate::queue::{
    Batch, BatchState, BatchView, EntryState, EntryView, QueueEntry, RepoQueueConfig,
};

/// A repo with its live queue depth, for the dashboard's repo switcher.
#[derive(Clone, Debug)]
pub struct RepoSummary {
    pub id: Uuid,
    pub owner: String,
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

    pub async fn repo_config<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
    ) -> Result<RepoQueueConfig, DbErr> {
        let m = entity::repo::Entity::find_by_id(repo_id)
            .one(c)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("repo {repo_id}")))?;
        Ok(RepoQueueConfig {
            repo_id,
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
        repo_id: Uuid,
    ) -> Result<Option<Batch>, DbErr> {
        let Some(m) = entity::batch::Entity::find()
            .filter(entity::batch::Column::RepoId.eq(repo_id))
            .filter(entity::batch::Column::State.is_in(ACTIVE))
            .one(c)
            .await?
        else {
            return Ok(None);
        };
        let entry_ids = Self::batch_entry_ids(c, m.id).await?;
        Ok(Some(Self::to_batch(m, entry_ids)))
    }

    /// The active batch projected for the FSM: entries in `ord` order, each with
    /// its PR number and per-PR `staged` progress. `None` if no batch is active.
    pub async fn active_batch_view<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
    ) -> Result<Option<BatchView>, DbErr> {
        let Some(m) = entity::batch::Entity::find()
            .filter(entity::batch::Column::RepoId.eq(repo_id))
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
            state: m.state,
            base_sha: m.base_sha,
            staging_sha: m.staging_sha,
            staging_ref: m.staging_ref,
            merge_blocked: m.merge_blocked,
            entries,
        }))
    }

    pub async fn next_queued<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        n: usize,
    ) -> Result<Vec<QueueEntry>, DbErr> {
        let rows = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
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
        entry_ids: &[Uuid],
        staging_ref: &str,
    ) -> Result<(), DbErr> {
        let id = Uuid::new_v4();
        entity::batch::ActiveModel {
            id: Set(id),
            repo_id: Set(repo_id),
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

    // --- api / ui ---

    pub async fn list_entries<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
    ) -> Result<Vec<QueueEntry>, DbErr> {
        let rows = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .order_by_asc(entity::queue_entry::Column::Position)
            .all(c)
            .await?;
        Ok(rows.into_iter().map(Self::to_entry).collect())
    }

    /// 1-based rank of `position` among the repo's OPEN entries. Counts entries
    /// strictly ahead, so a reported queue position stays honest even when dequeues
    /// have left gaps in the raw `position` sequence.
    pub async fn queue_rank<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        position: i32,
    ) -> Result<i64, DbErr> {
        let ahead = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .filter(entity::queue_entry::Column::Position.lt(position))
            .count(c)
            .await?;
        Ok(ahead as i64 + 1)
    }

    /// The PR number behind a queue entry id, or `None` if it's gone.
    pub async fn entry_pr_number<C: ConnectionTrait>(
        c: &C,
        entry_id: Uuid,
    ) -> Result<Option<u64>, DbErr> {
        Ok(entity::queue_entry::Entity::find_by_id(entry_id)
            .one(c)
            .await?
            .map(|m| m.pr_number as u64))
    }

    /// The open-queue state of a PR (`Queued` or `Testing`), or `None` if it isn't
    /// in the queue. Lets a dequeue command report accurately instead of blindly
    /// confirming a removal that didn't happen.
    pub async fn open_entry_state<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
    ) -> Result<Option<EntryState>, DbErr> {
        Ok(entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number as i64))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .one(c)
            .await?
            .map(|m| m.state))
    }

    /// The id of a PR's open queue entry (`Queued` or `Testing`), if any — lets a
    /// PR-keyed caller (the comment command) drive force-dequeue.
    pub async fn open_entry_id<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
    ) -> Result<Option<Uuid>, DbErr> {
        Ok(entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number as i64))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .one(c)
            .await?
            .map(|m| m.id))
    }

    pub async fn list_repos<C: ConnectionTrait>(c: &C) -> Result<Vec<RepoSummary>, DbErr> {
        let repos = entity::repo::Entity::find()
            .order_by_asc(entity::repo::Column::Owner)
            .order_by_asc(entity::repo::Column::Name)
            .all(c)
            .await?;
        let counts: Vec<(Uuid, i64)> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::State.eq(EntryState::Queued))
            .select_only()
            .column(entity::queue_entry::Column::RepoId)
            .column_as(entity::queue_entry::Column::Id.count(), "n")
            .group_by(entity::queue_entry::Column::RepoId)
            .into_tuple()
            .all(c)
            .await?;
        let by_repo: HashMap<Uuid, i64> = counts.into_iter().collect();
        Ok(repos
            .into_iter()
            .map(|r| RepoSummary {
                queued: by_repo.get(&r.id).copied().unwrap_or(0),
                id: r.id,
                owner: r.owner,
                name: r.name,
                base_branch: r.base_branch,
                batch_size: r.batch_size,
            })
            .collect())
    }

    pub async fn active_repo_ids<C: ConnectionTrait>(c: &C) -> Result<Vec<Uuid>, DbErr> {
        entity::repo::Entity::find()
            .select_only()
            .column(entity::repo::Column::Id)
            .into_tuple()
            .all(c)
            .await
    }

    pub async fn enqueue<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        pr_number: u64,
        head_sha: &str,
        by: &str,
    ) -> Result<QueueEntry, DbErr> {
        if let Some(m) = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .filter(entity::queue_entry::Column::PrNumber.eq(pr_number as i64))
            .filter(entity::queue_entry::Column::State.is_in(OPEN))
            .one(c)
            .await?
        {
            return Ok(Self::to_entry(m));
        }
        let max_pos: Option<i32> = entity::queue_entry::Entity::find()
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
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

    /// Remove a queued entry from a repo. Returns whether a row was actually
    /// deleted (only `Queued` entries in this repo are removable), so the caller
    /// only announces a removal that really happened.
    pub async fn dequeue<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        entry_id: Uuid,
    ) -> Result<bool, DbErr> {
        let res = entity::queue_entry::Entity::delete_many()
            .filter(entity::queue_entry::Column::Id.eq(entry_id))
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
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
        repo_id: Uuid,
        entry_id: Uuid,
    ) -> Result<bool, DbErr> {
        let res = entity::queue_entry::Entity::delete_many()
            .filter(entity::queue_entry::Column::Id.eq(entry_id))
            .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
            .exec(c)
            .await?;
        Ok(res.rows_affected > 0)
    }

    pub async fn reorder<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        ordered: &[Uuid],
    ) -> Result<(), DbErr> {
        for (pos, &id) in ordered.iter().enumerate() {
            entity::queue_entry::Entity::update_many()
                .col_expr(
                    entity::queue_entry::Column::Position,
                    Expr::value(pos as i32),
                )
                .filter(entity::queue_entry::Column::Id.eq(id))
                .filter(entity::queue_entry::Column::RepoId.eq(repo_id))
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
        Ok(())
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

    /// Set a repo's base branch (synced from the repo's GitHub default branch).
    pub async fn set_base_branch<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<(), DbErr> {
        entity::repo::ActiveModel {
            id: Set(repo_id),
            base_branch: Set(branch.to_owned()),
            ..Default::default()
        }
        .update(c)
        .await?;
        Ok(())
    }

    /// Set a repo's required check contexts (synced from GitHub branch protection).
    /// These are the contexts the engine gates a batch on; empty holds the queue.
    pub async fn set_required_checks<C: ConnectionTrait>(
        c: &C,
        repo_id: Uuid,
        checks: &[String],
    ) -> Result<(), DbErr> {
        entity::repo::ActiveModel {
            id: Set(repo_id),
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
