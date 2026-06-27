//! Multi-queue support: a repo may host N independent named queues. The queue now
//! owns the config (the five columns move off `repos`), and `queue_entries`,
//! `batches`, and `queue_ledger` each gain a `queue_id`. The singletons re-key onto
//! the queue: `batches_one_active` becomes one active batch per QUEUE, and the
//! per-queue ordering index moves to `(queue_id, position)`. `queue_entries_repo_pr_open`
//! stays repo-wide — a PR is open in at most one queue per repo (prevents double-merge).
//!
//! Backfill (this is a new system, so it only keeps the dev rows coherent): create
//! one `default` queue per repo copying its five config values, stamp every existing
//! entry/batch/ledger row, then enforce NOT NULL + FK and drop the moved columns.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const QUEUES: &str = "queues";
const REPOS: &str = "repos";
const QUEUE_ENTRIES: &str = "queue_entries";
const BATCHES: &str = "batches";
const QUEUE_LEDGER: &str = "queue_ledger";

/// The child tables that gain a `queue_id`, paired with their FK constraint name.
const CHILDREN: [(&str, &str); 3] = [
    (QUEUE_ENTRIES, "queue_entries_queue_id_fkey"),
    (BATCHES, "batches_queue_id_fkey"),
    (QUEUE_LEDGER, "queue_ledger_queue_id_fkey"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new(QUEUES))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .uuid()
                            .not_null()
                            .default(Expr::cust("gen_random_uuid()"))
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("repo_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("name")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("base_branch"))
                            .text()
                            .not_null()
                            .default("main"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("batch_size"))
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(Alias::new("merge_method"))
                            .custom(Alias::new("merge_method"))
                            .not_null()
                            .default("squash"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("staging_prefix"))
                            .text()
                            .not_null()
                            .default("mq/staging"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("required_checks"))
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'[]'::jsonb")),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::cust("now()")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new(QUEUES), Alias::new("repo_id"))
                            .to(Alias::new(REPOS), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .col(Alias::new("repo_id"))
                            .col(Alias::new("name"))
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        for (table, _) in CHILDREN {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .add_column(ColumnDef::new(Alias::new("queue_id")).uuid().null())
                        .to_owned(),
                )
                .await?;
        }

        Self::backfill(manager).await?;

        for (table, fk) in CHILDREN {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .modify_column(ColumnDef::new(Alias::new("queue_id")).uuid().not_null())
                        .to_owned(),
                )
                .await?;
            manager
                .create_foreign_key(
                    ForeignKey::create()
                        .name(fk)
                        .from(Alias::new(table), Alias::new("queue_id"))
                        .to(Alias::new(QUEUES), Alias::new("id"))
                        .on_delete(ForeignKeyAction::Cascade)
                        .to_owned(),
                )
                .await?;
        }

        manager
            .drop_index(
                Index::drop()
                    .name("queue_entries_repo_pos")
                    .table(Alias::new(QUEUE_ENTRIES))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("queue_entries_queue_pos")
                    .table(Alias::new(QUEUE_ENTRIES))
                    .col(Alias::new("queue_id"))
                    .col(Alias::new("position"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("batches_one_active")
                    .table(Alias::new(BATCHES))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("batches_one_active")
                    .table(Alias::new(BATCHES))
                    .col(Alias::new("queue_id"))
                    .unique()
                    .cond_where(Expr::cust(
                        "state IN ('staging', 'testing', 'merging', 'bisecting')",
                    ))
                    .to_owned(),
            )
            .await?;

        for col in [
            "base_branch",
            "batch_size",
            "merge_method",
            "staging_prefix",
            "required_checks",
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(REPOS))
                        .drop_column(Alias::new(col))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new(REPOS))
                    .add_column(
                        ColumnDef::new(Alias::new("base_branch"))
                            .text()
                            .not_null()
                            .default("main"),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("batch_size"))
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("merge_method"))
                            .custom(Alias::new("merge_method"))
                            .not_null()
                            .default("squash"),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("staging_prefix"))
                            .text()
                            .not_null()
                            .default("mq/staging"),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("required_checks"))
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'[]'::jsonb")),
                    )
                    .to_owned(),
            )
            .await?;

        for (table, fk) in CHILDREN {
            manager
                .drop_foreign_key(
                    ForeignKey::drop()
                        .name(fk)
                        .table(Alias::new(table))
                        .to_owned(),
                )
                .await?;
        }

        manager
            .drop_index(
                Index::drop()
                    .name("batches_one_active")
                    .table(Alias::new(BATCHES))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("batches_one_active")
                    .table(Alias::new(BATCHES))
                    .col(Alias::new("repo_id"))
                    .unique()
                    .cond_where(Expr::cust(
                        "state IN ('staging', 'testing', 'merging', 'bisecting')",
                    ))
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("queue_entries_queue_pos")
                    .table(Alias::new(QUEUE_ENTRIES))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("queue_entries_repo_pos")
                    .table(Alias::new(QUEUE_ENTRIES))
                    .col(Alias::new("repo_id"))
                    .col(Alias::new("position"))
                    .to_owned(),
            )
            .await?;

        for (table, _) in CHILDREN {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .drop_column(Alias::new("queue_id"))
                        .to_owned(),
                )
                .await?;
        }

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new(QUEUES))
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

impl Migration {
    /// Seed one `default` queue per repo from its config columns, then stamp every
    /// existing entry/batch/ledger row with its repo's default queue. Built with the
    /// sea-query query builder (INSERT…SELECT + correlated UPDATEs) so the migration
    /// stays on the builder API rather than raw SQL.
    async fn backfill(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = manager.get_database_backend();

        let select = Query::select()
            .expr(Expr::cust("gen_random_uuid()"))
            .column(Alias::new("id"))
            .expr(Expr::val("default"))
            .column(Alias::new("base_branch"))
            .column(Alias::new("batch_size"))
            .column(Alias::new("merge_method"))
            .column(Alias::new("staging_prefix"))
            .column(Alias::new("required_checks"))
            .from(Alias::new(REPOS))
            .to_owned();
        let mut insert = Query::insert();
        insert.into_table(Alias::new(QUEUES)).columns([
            Alias::new("id"),
            Alias::new("repo_id"),
            Alias::new("name"),
            Alias::new("base_branch"),
            Alias::new("batch_size"),
            Alias::new("merge_method"),
            Alias::new("staging_prefix"),
            Alias::new("required_checks"),
        ]);
        insert
            .select_from(select)
            .map_err(|e| DbErr::Custom(format!("backfill queues: {e}")))?;
        db.execute(backend.build(&insert)).await?;

        for table in [QUEUE_ENTRIES, BATCHES, QUEUE_LEDGER] {
            let subquery = Query::select()
                .column((Alias::new(QUEUES), Alias::new("id")))
                .from(Alias::new(QUEUES))
                .and_where(
                    Expr::col((Alias::new(QUEUES), Alias::new("repo_id")))
                        .equals((Alias::new(table), Alias::new("repo_id"))),
                )
                .to_owned();
            let update = Query::update()
                .table(Alias::new(table))
                .value(
                    Alias::new("queue_id"),
                    SimpleExpr::SubQuery(None, Box::new(subquery.into_sub_query_statement())),
                )
                .to_owned();
            db.execute(backend.build(&update)).await?;
        }

        Ok(())
    }
}
