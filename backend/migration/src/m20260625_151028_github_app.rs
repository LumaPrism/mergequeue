//! Credentials of the GitHub App this instance owns. Populated by the manifest
//! setup flow (POST /app-manifests/{code}/conversions). Single row (id = 1) — one
//! App per mergequeue instance. The App client is built lazily from this row, so
//! setup takes effect without a restart.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("github_app"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .default(1)
                            .primary_key()
                            .check(Expr::cust("id = 1")),
                    )
                    .col(
                        ColumnDef::new(Alias::new("app_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("slug")).text().not_null())
                    .col(ColumnDef::new(Alias::new("client_id")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("client_secret"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("private_key")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("webhook_secret"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("html_url")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::cust("now()")),
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
                    .table(Alias::new("github_app"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
