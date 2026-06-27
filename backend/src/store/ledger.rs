//! The append-only batch-outcome ledger.

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;

use super::entity;
use crate::queue::LedgerRecord;

/// Ledger persistence. Zero-sized; all behavior is associated functions.
#[derive(Debug, Clone, Copy)]
pub struct LedgerStore;

impl LedgerStore {
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
}
