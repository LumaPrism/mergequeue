//! SeaORM entities for the queue schema (see `migration/`). The three Postgres
//! enums are the domain enums in `queue::model`, which derive `DeriveActiveEnum`;
//! `required_checks` is a typed JSONB column via `FromJsonQueryResult`.

use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};

/// The repo's required check contexts, stored as a JSONB string array.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct RequiredChecks(pub Vec<String>);

pub mod installation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "installations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(unique)]
        pub installation_id: i64,
        pub account_login: String,
        pub status: String,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod repo {
    use sea_orm::entity::prelude::*;

    use super::RequiredChecks;
    use crate::queue::MergeMethod;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "repos")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub installation_pk: Uuid,
        pub owner: String,
        pub name: String,
        pub base_branch: String,
        pub batch_size: i32,
        pub merge_method: MergeMethod,
        pub staging_prefix: String,
        pub required_checks: RequiredChecks,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod queue_entry {
    use sea_orm::entity::prelude::*;

    use crate::queue::EntryState;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "queue_entries")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub repo_id: Uuid,
        pub pr_number: i64,
        pub position: i32,
        pub state: EntryState,
        pub enqueued_by: String,
        pub enqueued_at: DateTimeWithTimeZone,
        pub head_sha: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod batch {
    use sea_orm::entity::prelude::*;

    use crate::queue::BatchState;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "batches")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub repo_id: Uuid,
        pub base_sha: String,
        pub staging_sha: Option<String>,
        pub staging_ref: String,
        pub state: BatchState,
        pub merge_blocked: bool,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod batch_entry {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "batch_entries")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub batch_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub entry_id: Uuid,
        pub ord: i32,
        pub staged: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
