//! The merge queue: domain model + the engine that stages, tests, and lands
//! batches. See `CLAUDE.md` → "The core idea".

mod engine;
mod model;
mod state;

pub use engine::Engine;
pub use model::{
    Batch, BatchState, EntryState, MergeMethod, QueueEntry, RepoQueueConfig, TickOutcome,
};
pub use state::{
    BatchView, DbWrite, Decision, Effect, EntryView, Fact, Flow, GhCall, MergeReport, Observation,
    State,
};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("github: {0}")]
    GitHub(#[from] crate::github::GitHubError),

    #[error("store: {0}")]
    Store(#[from] sea_orm::DbErr),

    #[error("engine: {0}")]
    Other(String),
}
