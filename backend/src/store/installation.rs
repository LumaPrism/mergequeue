//! Installation + repo lifecycle: provision/deprovision and reconcile-prune.

use chrono::{DateTime, FixedOffset};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use super::entity;

/// Installation lifecycle persistence. Zero-sized; all behavior is associated
/// functions.
#[derive(Debug, Clone, Copy)]
pub struct InstallationStore;

impl InstallationStore {
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
}
