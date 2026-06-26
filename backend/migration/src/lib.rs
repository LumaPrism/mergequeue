pub use sea_orm_migration::prelude::*;

mod m20260625_131611_init;
mod m20260625_151028_github_app;
mod m20260625_165821_auth;
mod m20260626_111218_staging_progress;
mod m20260626_115024_merge_blocked;
mod m20260626_182440_queue_ledger;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260625_131611_init::Migration),
            Box::new(m20260625_151028_github_app::Migration),
            Box::new(m20260625_165821_auth::Migration),
            Box::new(m20260626_111218_staging_progress::Migration),
            Box::new(m20260626_115024_merge_blocked::Migration),
            Box::new(m20260626_182440_queue_ledger::Migration),
        ]
    }
}
