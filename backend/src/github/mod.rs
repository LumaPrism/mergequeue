//! GitHub App integration: authenticate as the App, mint per-installation
//! clients, and expose the repo operations the engine needs behind a trait so
//! the engine can be unit-tested against a mock.

mod app;
mod client;

pub use app::{AppClient, AppCredentials};
pub use client::{
    CheckState, GitHubRepoClient, MergeOutcome, PullSummary, RepoClient, RepoId, RepoPermission,
    TrainLabel,
};

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    /// Boxed because `octocrab::Error` is large — keeps `Result<_, GitHubError>`
    /// small (clippy::result_large_err).
    #[error("github api: {0}")]
    Api(Box<octocrab::Error>),

    #[error("jwt key: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A bare HTTP status with no backing octocrab error — lets callers react to a
    /// known status (e.g. a hand-rolled REST call, or a synthetic error in tests).
    #[error("github status {0}")]
    Status(u16),

    #[error("github: {0}")]
    Other(String),
}

impl From<octocrab::Error> for GitHubError {
    fn from(e: octocrab::Error) -> Self {
        GitHubError::Api(Box::new(e))
    }
}

impl GitHubError {
    /// The HTTP status of a GitHub API error, if this is one. Lets the engine tell
    /// a rejected ref update (422/403 — branch protection / not-a-fast-forward)
    /// apart from a transient failure.
    pub fn status(&self) -> Option<u16> {
        match self {
            GitHubError::Api(e) => match e.as_ref() {
                octocrab::Error::GitHub { source, .. } => Some(source.status_code.as_u16()),
                _ => None,
            },
            GitHubError::Status(code) => Some(*code),
            GitHubError::Jwt(_) | GitHubError::Io(_) | GitHubError::Other(_) => None,
        }
    }
}
