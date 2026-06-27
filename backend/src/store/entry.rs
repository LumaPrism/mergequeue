//! Queue-entry persistence: enqueue/dequeue/remove/requeue/reorder, listing,
//! ranking, open-entry queries, and entry-state management.

use std::collections::HashMap;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;

use super::{OPEN, entity};
use crate::queue::{EntryState, QueueEntry};

/// Queue-entry persistence. Zero-sized; all behavior is associated functions.
#[derive(Debug, Clone, Copy)]
pub struct EntryStore;

impl EntryStore {
    pub(crate) fn to_entry(m: entity::queue_entry::Model) -> QueueEntry {
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
