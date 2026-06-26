//! Top-level error. Each module owns its own `thiserror` enum and they flow up
//! here via `#[from]` so `?` propagates cleanly.

use crate::github::GitHubError;
use crate::queue::EngineError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),

    #[error("github error: {0}")]
    GitHub(#[from] GitHubError),

    #[error("engine error: {0}")]
    Engine(#[from] EngineError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
