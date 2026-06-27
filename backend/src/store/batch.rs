//! Batch persistence: the active-batch reads/projection, batch creation, the next
//! queued slice, and the FSM batch-state mutators.

use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use uuid::Uuid;

use super::{ACTIVE, EntryStore, entity};
use crate::queue::{Batch, BatchState, BatchView, EntryState, EntryView, QueueEntry};

/// Batch persistence. Zero-sized; all behavior is associated functions.
#[derive(Debug, Clone, Copy)]
pub struct BatchStore;

impl BatchStore {
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
        Ok(rows.into_iter().map(EntryStore::to_entry).collect())
    }

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
}
