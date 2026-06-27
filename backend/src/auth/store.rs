//! Auth persistence. `AuthStore` is zero-sized; all behavior is associated
//! functions over the `users`/`sessions` SeaORM entities defined here. A session
//! is an opaque row id looked up (and expiry-checked) on each request.

use chrono::{Duration, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set,
};
use uuid::Uuid;

pub mod user {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(unique)]
        pub github_id: i64,
        pub login: String,
        pub avatar_url: String,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod session {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "sessions")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_pk: Uuid,
        pub created_at: DateTimeWithTimeZone,
        pub expires_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Auth persistence. Zero-sized; all behavior is associated functions.
pub struct AuthStore;

impl AuthStore {
    /// How long a session lives — drives both the row's `expires_at` and the
    /// cookie's `Max-Age`.
    pub const SESSION_DAYS: i64 = 30;

    /// The authenticated user behind a session id, if the session exists and is
    /// unexpired.
    pub async fn current_user(
        db: &DatabaseConnection,
        session_id: Uuid,
    ) -> Result<Option<user::Model>, DbErr> {
        let Some(s) = session::Entity::find_by_id(session_id).one(db).await? else {
            return Ok(None);
        };
        if s.expires_at.with_timezone(&Utc) < Utc::now() {
            return Ok(None);
        }
        user::Entity::find_by_id(s.user_pk).one(db).await
    }

    /// Insert or update the user keyed by `github_id`, returning the internal pk.
    pub async fn upsert_user(
        db: &DatabaseConnection,
        github_id: i64,
        gh_login: &str,
        avatar_url: &str,
    ) -> Result<Uuid, DbErr> {
        user::Entity::insert(user::ActiveModel {
            id: Set(Uuid::new_v4()),
            github_id: Set(github_id),
            login: Set(gh_login.to_owned()),
            avatar_url: Set(avatar_url.to_owned()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(user::Column::GithubId)
                .update_columns([user::Column::Login, user::Column::AvatarUrl])
                .to_owned(),
        )
        .exec(db)
        .await?;
        let row = user::Entity::find()
            .filter(user::Column::GithubId.eq(github_id))
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("user upsert lost".into()))?;
        Ok(row.id)
    }

    /// Open a new session for `user_pk`, returning its opaque id.
    pub async fn create_session(db: &DatabaseConnection, user_pk: Uuid) -> Result<Uuid, DbErr> {
        let id = Uuid::new_v4();
        let expires = Utc::now() + Duration::days(Self::SESSION_DAYS);
        session::ActiveModel {
            id: Set(id),
            user_pk: Set(user_pk),
            expires_at: Set(expires.fixed_offset()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(id)
    }
}
