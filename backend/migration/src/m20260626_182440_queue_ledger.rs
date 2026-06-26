//! Append-only ledger that records the terminal outcome of every batch.

use sea_orm_migration::prelude::extension::postgres::Type;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_type(
                Type::create()
                    .as_enum(Alias::new("ledger_outcome"))
                    .values([
                        Alias::new("merged"),
                        Alias::new("ejected"),
                        Alias::new("superseded"),
                    ])
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Alias::new("queue_ledger"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .uuid()
                            .not_null()
                            .default(Expr::cust("gen_random_uuid()"))
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("repo_id")).uuid().not_null())
                    .col(
                        ColumnDef::new(Alias::new("batch_id"))
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("outcome"))
                            .custom(Alias::new("ledger_outcome"))
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("base_sha")).text().not_null())
                    .col(ColumnDef::new(Alias::new("landed_sha")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("ejected_pr"))
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("entries"))
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ended_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("queue_ledger"), Alias::new("repo_id"))
                            .to(Alias::new("repos"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("queue_ledger"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_type(
                Type::drop()
                    .if_exists()
                    .name(Alias::new("ledger_outcome"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
