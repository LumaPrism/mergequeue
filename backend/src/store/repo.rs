//! Repo persistence: upsert/delete/list, name/id resolution, GitHub identity, and
//! the repo-summary projection.

use std::collections::HashMap;

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;

use super::{QueueStore, QueueSummary, entity};
use crate::github::RepoId;
use crate::queue::EntryState;

/// A repo with its named queues, for the dashboard's switcher.
#[derive(Clone, Debug)]
pub struct RepoSummary {
    pub id: Uuid,
    pub owner: String,
    pub name: String,
    pub queues: Vec<QueueSummary>,
}

/// Repo persistence. Zero-sized; all behavior is associated functions.
#[derive(Debug, Clone, Copy)]
pub struct RepoStore;

impl RepoStore {
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
            QueueStore::get_or_create_queue(c, repo_id, "default").await?;
        }
        Ok(())
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
}
