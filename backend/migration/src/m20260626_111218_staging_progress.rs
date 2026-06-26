//! Per-PR staging progress for the pure-FSM engine: `batch_entries.staged` marks
//! a PR whose head is durably merged onto the staging branch, so a resumed stage
//! continues from where it left off instead of rebuilding from scratch.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE batch_entries ADD COLUMN staged BOOLEAN NOT NULL DEFAULT false;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE batch_entries DROP COLUMN IF EXISTS staged;")
            .await?;
        Ok(())
    }
}
