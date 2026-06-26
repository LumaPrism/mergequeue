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
            .alter_table(
                Table::alter()
                    .table(Alias::new("batch_entries"))
                    .add_column(
                        ColumnDef::new(Alias::new("staged"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("batch_entries"))
                    .drop_column(Alias::new("staged"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
