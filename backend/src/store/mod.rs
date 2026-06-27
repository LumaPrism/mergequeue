//! Persistence. Each per-domain store is a set of connection-generic associated
//! functions over the SeaORM entities in `entity`. Every function takes
//! `&C: ConnectionTrait`, so the *caller* owns the transaction boundary (a
//! `&DatabaseConnection` for a single autocommit statement, or a
//! `&DatabaseTransaction` to make several atomic) — the store never opens one
//! itself. Functions return the storage-agnostic domain types from `queue::model`.

mod entity;

pub use entity::queue_ledger;

mod batch;
mod entry;
mod installation;
mod ledger;
mod queue;
mod repo;

pub use batch::*;
pub use entry::*;
pub use installation::*;
pub use ledger::*;
pub use queue::*;
pub use repo::*;

use crate::queue::{BatchState, EntryState};

const ACTIVE: [BatchState; 4] = [
    BatchState::Staging,
    BatchState::Testing,
    BatchState::Merging,
    BatchState::Bisecting,
];
const OPEN: [EntryState; 2] = [EntryState::Queued, EntryState::Testing];

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use chrono::{Duration, Utc};
    use migration::{Migrator, MigratorTrait};
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Set,
        Statement,
    };
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    use super::{
        BatchStore, EntryStore, InstallationStore, LedgerStore, QueueStore, RepoStore, entity,
    };
    use crate::queue::{LedgerEntry, LedgerEntryResult, LedgerOutcome, LedgerRecord};

    /// Serializes DB tests against the shared test database.
    static DB_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    async fn test_db() -> DatabaseConnection {
        let url = std::env::var("MQ_TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5433/mergequeue_test".into()
        });
        let (host, _) = url.rsplit_once('/').expect("db url has a path");
        if let Ok(maint) = Database::connect(format!("{host}/postgres")).await {
            let _ = maint
                .execute(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "CREATE DATABASE mergequeue_test",
                ))
                .await;
        }
        let db = Database::connect(&url).await.expect("connect test db");
        Migrator::up(&db, None).await.expect("migrate test db");
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "TRUNCATE queue_ledger, batch_entries, batches, queue_entries, queues, repos, \
             installations CASCADE",
        ))
        .await
        .unwrap();
        db
    }

    /// Seed a repo and return `(repo_id, default_queue_id)` — `upsert_repo` creates
    /// the `default` queue.
    async fn seed_repo(db: &DatabaseConnection) -> (Uuid, Uuid) {
        InstallationStore::provision_installation(db, 77, "acme")
            .await
            .unwrap();
        RepoStore::upsert_repo(db, 77, "acme", "widgets")
            .await
            .unwrap();
        let repo_id = RepoStore::repo_id_by_name(db, "acme", "widgets")
            .await
            .unwrap()
            .unwrap();
        let queue_id = QueueStore::queue_id_by_name(db, repo_id, "default")
            .await
            .unwrap()
            .unwrap();
        (repo_id, queue_id)
    }

    fn record(
        repo_id: Uuid,
        queue_id: Uuid,
        batch_id: Uuid,
        outcome: LedgerOutcome,
    ) -> LedgerRecord {
        LedgerRecord {
            batch_id,
            repo_id,
            queue_id,
            outcome,
            base_sha: "base000".into(),
            landed_sha: Some("stg777".into()),
            ejected_pr: None,
            entries: vec![
                LedgerEntry {
                    pr_number: 101,
                    result: LedgerEntryResult::Landed,
                },
                LedgerEntry {
                    pr_number: 102,
                    result: LedgerEntryResult::Requeued,
                },
            ],
            started_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_store_ledger_round_trips_one_record() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let rec = record(repo_id, queue_id, Uuid::new_v4(), LedgerOutcome::Merged);
        LedgerStore::append_ledger(&db, &rec).await.unwrap();

        let rows = LedgerStore::list_ledger(&db, queue_id, 50).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.batch_id, rec.batch_id);
        assert_eq!(row.outcome, LedgerOutcome::Merged);
        assert_eq!(row.landed_sha.as_deref(), Some("stg777"));
        assert!(row.ejected_pr.is_none());
        let entries: Vec<LedgerEntry> = serde_json::from_value(row.entries.clone()).unwrap();
        assert_eq!(entries, rec.entries);
    }

    #[tokio::test]
    async fn test_store_ledger_append_twice_is_idempotent() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let rec = record(repo_id, queue_id, Uuid::new_v4(), LedgerOutcome::Ejected);
        LedgerStore::append_ledger(&db, &rec).await.unwrap();
        LedgerStore::append_ledger(&db, &rec).await.unwrap();
        let rows = LedgerStore::list_ledger(&db, queue_id, 50).await.unwrap();
        assert_eq!(rows.len(), 1, "a duplicate batch_id append must be a no-op");
    }

    #[tokio::test]
    async fn test_store_ledger_list_orders_newest_first() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, queue_id) = seed_repo(&db).await;
        let base = Utc::now();
        let mut ids = Vec::new();
        for i in 0..3i64 {
            let id = Uuid::new_v4();
            ids.push(id);
            entity::queue_ledger::ActiveModel {
                id: Set(id),
                repo_id: Set(repo_id),
                queue_id: Set(queue_id),
                batch_id: Set(Uuid::new_v4()),
                outcome: Set(LedgerOutcome::Merged),
                base_sha: Set("base000".into()),
                landed_sha: Set(None),
                ejected_pr: Set(None),
                entries: Set(serde_json::json!([])),
                started_at: Set(base.into()),
                ended_at: Set((base + Duration::seconds(i)).into()),
            }
            .insert(&db)
            .await
            .unwrap();
        }
        let rows = LedgerStore::list_ledger(&db, queue_id, 2).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, ids[2], "newest row comes first");
        assert_eq!(rows[1].id, ids[1]);
    }

    #[tokio::test]
    async fn test_store_get_or_create_queue_is_idempotent() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let again = QueueStore::get_or_create_queue(&db, repo_id, "default")
            .await
            .unwrap();
        assert_eq!(
            again, default_id,
            "default queue resolves, never duplicates"
        );
        let fe1 = QueueStore::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();
        let fe2 = QueueStore::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();
        assert_eq!(fe1, fe2, "a named queue is created once, then resolved");
        assert_ne!(fe1, default_id);
        let queues = QueueStore::list_queues(&db, repo_id).await.unwrap();
        assert_eq!(queues.len(), 2, "default + frontend");
    }

    #[tokio::test]
    async fn test_store_active_batch_is_isolated_per_queue() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let fe_id = QueueStore::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();

        let a = EntryStore::enqueue(&db, repo_id, default_id, 101, "h101", "alice")
            .await
            .unwrap();
        let b = EntryStore::enqueue(&db, repo_id, fe_id, 202, "h202", "bob")
            .await
            .unwrap();
        BatchStore::create_batch(&db, repo_id, default_id, &[a.id], "mq/staging/default/main")
            .await
            .unwrap();
        BatchStore::create_batch(&db, repo_id, fe_id, &[b.id], "mq/staging/frontend/main")
            .await
            .unwrap();

        let da = BatchStore::active_batch_view(&db, default_id)
            .await
            .unwrap()
            .expect("default queue has an active batch");
        let fb = BatchStore::active_batch_view(&db, fe_id)
            .await
            .unwrap()
            .expect("frontend queue has an active batch");
        assert_ne!(
            da.id, fb.id,
            "each queue in the repo holds its own active batch concurrently"
        );
        assert_eq!(da.queue_id, default_id);
        assert_eq!(fb.queue_id, fe_id);
        assert_eq!(da.prs(), vec![101]);
        assert_eq!(fb.prs(), vec![202]);
    }

    #[tokio::test]
    async fn test_store_enqueue_pr_open_guard_is_repo_wide() {
        let _guard = DB_LOCK.lock().await;
        let db = test_db().await;
        let (repo_id, default_id) = seed_repo(&db).await;
        let fe_id = QueueStore::get_or_create_queue(&db, repo_id, "frontend")
            .await
            .unwrap();

        EntryStore::enqueue(&db, repo_id, default_id, 303, "h303", "carol")
            .await
            .unwrap();
        let dup = EntryStore::enqueue(&db, repo_id, fe_id, 303, "h303", "carol").await;
        assert!(
            dup.is_err(),
            "the repo-wide PR-open guard rejects the same PR open in a second queue"
        );
        assert_eq!(
            EntryStore::list_entries(&db, default_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            EntryStore::list_entries(&db, fe_id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
