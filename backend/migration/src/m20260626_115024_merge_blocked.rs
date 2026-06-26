//! `batches.merge_blocked` — set once when the engine's fast-forward of the base
//! branch is rejected (e.g. a ruleset requires a PR and the App isn't a bypass
//! actor), so the operator is told once (comment + label) instead of the engine
//! retrying the rejected push silently forever.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE batches ADD COLUMN merge_blocked BOOLEAN NOT NULL DEFAULT false;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE batches DROP COLUMN IF EXISTS merge_blocked;")
            .await?;
        Ok(())
    }
}
