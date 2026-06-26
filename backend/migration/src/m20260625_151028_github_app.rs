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
        let sql = r#"
        CREATE TABLE github_app (
            id             INT  PRIMARY KEY DEFAULT 1 CHECK (id = 1),
            app_id         BIGINT NOT NULL,
            slug           TEXT NOT NULL,
            client_id      TEXT NOT NULL,
            client_secret  TEXT NOT NULL,
            private_key    TEXT NOT NULL,
            webhook_secret TEXT NOT NULL,
            html_url       TEXT,
            created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        "#;
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS github_app;")
            .await?;
        Ok(())
    }
}
