//! Auth: GitHub-identified users and their sessions. Login is GitHub-only (the
//! App's OAuth), so a user is keyed by `github_id`; a session is an opaque token
//! (the row id) carried in an httpOnly cookie.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
        CREATE TABLE users (
            id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            github_id  BIGINT NOT NULL UNIQUE,
            login      TEXT   NOT NULL,
            avatar_url TEXT   NOT NULL DEFAULT '',
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE TABLE sessions (
            id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            user_pk    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at TIMESTAMPTZ NOT NULL
        );
        CREATE INDEX sessions_user ON sessions (user_pk);
        "#;
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
        DROP TABLE IF EXISTS sessions;
        DROP TABLE IF EXISTS users;
        "#;
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }
}
