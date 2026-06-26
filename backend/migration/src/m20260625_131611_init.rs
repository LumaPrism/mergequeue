//! Initial schema. Raw SQL for readability; SeaORM entities are generated from
//! this. One installation has many repos; each repo has a queue (entries) and at
//! most one non-terminal batch.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
        CREATE TYPE entry_state  AS ENUM ('queued', 'testing', 'merged', 'ejected');
        CREATE TYPE batch_state  AS ENUM ('staging', 'testing', 'merging', 'bisecting', 'merged', 'ejected', 'superseded');
        CREATE TYPE merge_method AS ENUM ('squash', 'merge', 'rebase');

        CREATE TABLE installations (
            id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            installation_id BIGINT NOT NULL UNIQUE,
            account_login   TEXT   NOT NULL,
            status          TEXT   NOT NULL DEFAULT 'active',
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE TABLE repos (
            id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            installation_pk UUID NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
            owner           TEXT NOT NULL,
            name            TEXT NOT NULL,
            base_branch     TEXT NOT NULL DEFAULT 'main',
            batch_size      INT  NOT NULL DEFAULT 1,
            merge_method    merge_method NOT NULL DEFAULT 'squash',
            staging_prefix  TEXT NOT NULL DEFAULT 'mq/staging',
            required_checks JSONB NOT NULL DEFAULT '[]',
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (owner, name)
        );

        CREATE TABLE queue_entries (
            id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id     UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            pr_number   BIGINT NOT NULL,
            position    INT NOT NULL,
            state       entry_state NOT NULL DEFAULT 'queued',
            enqueued_by TEXT NOT NULL,
            enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            head_sha    TEXT NOT NULL
        );
        CREATE INDEX queue_entries_repo_pos ON queue_entries (repo_id, position);
        CREATE UNIQUE INDEX queue_entries_repo_pr_open
            ON queue_entries (repo_id, pr_number)
            WHERE state IN ('queued', 'testing');

        CREATE TABLE batches (
            id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id     UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            base_sha    TEXT NOT NULL,
            staging_sha TEXT,
            staging_ref TEXT NOT NULL,
            state       batch_state NOT NULL DEFAULT 'staging',
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        -- at most one non-terminal batch per repo (FSM invariant)
        CREATE UNIQUE INDEX batches_one_active
            ON batches (repo_id)
            WHERE state IN ('staging', 'testing', 'merging', 'bisecting');

        CREATE TABLE batch_entries (
            batch_id UUID NOT NULL REFERENCES batches(id) ON DELETE CASCADE,
            entry_id UUID NOT NULL REFERENCES queue_entries(id) ON DELETE CASCADE,
            ord      INT  NOT NULL,
            PRIMARY KEY (batch_id, entry_id)
        );
        "#;
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
        DROP TABLE IF EXISTS batch_entries;
        DROP TABLE IF EXISTS batches;
        DROP TABLE IF EXISTS queue_entries;
        DROP TABLE IF EXISTS repos;
        DROP TABLE IF EXISTS installations;
        DROP TYPE IF EXISTS merge_method;
        DROP TYPE IF EXISTS batch_state;
        DROP TYPE IF EXISTS entry_state;
        "#;
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }
}
