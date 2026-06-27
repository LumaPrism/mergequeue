//! Queue persistence: config, create/get/list, name/id resolution, settings, the
//! active-queue scan, and the queue-summary projection.

use std::collections::{BTreeSet, HashMap};

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use uuid::Uuid;

use super::{ACTIVE, OPEN, entity};
use crate::queue::{EntryState, MergeMethod, RepoQueueConfig};

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

/// Queue persistence. Zero-sized; all behavior is associated functions.
#[derive(Debug, Clone, Copy)]
pub struct QueueStore;

impl QueueStore {
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
}
