//! Domain types for the queue. These are storage-agnostic; `store` maps them to
//! SeaORM entities.

use chrono::{DateTime, Utc};
use poem_openapi::Enum;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use typeshare::typeshare;
use uuid::Uuid;

/// Lifecycle of a single queued PR.
#[typeshare]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum, EnumIter, DeriveActiveEnum,
)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "entry_state")]
pub enum EntryState {
    /// Waiting in line.
    #[sea_orm(string_value = "queued")]
    Queued,
    /// Part of the batch currently being staged/tested.
    #[sea_orm(string_value = "testing")]
    Testing,
    /// Landed on the base branch.
    #[sea_orm(string_value = "merged")]
    Merged,
    /// Removed from the queue after being identified as the batch breaker.
    #[sea_orm(string_value = "ejected")]
    Ejected,
}

impl EntryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Testing => "testing",
            Self::Merged => "merged",
            Self::Ejected => "ejected",
        }
    }

    /// Parse the Postgres `entry_state` text. `None` on an unknown value.
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "testing" => Self::Testing,
            "merged" => Self::Merged,
            "ejected" => Self::Ejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub queue_id: Uuid,
    pub pr_number: u64,
    /// Ordering within the queue. Lower merges first.
    pub position: i32,
    pub state: EntryState,
    pub enqueued_by: String,
    pub enqueued_at: DateTime<Utc>,
    /// PR head sha when enqueued; used to detect a PR being updated mid-flight.
    pub head_sha: String,
}

/// Batch lifecycle as an explicit finite state machine. The value is persisted
/// to `batches.state`; the engine resumes after a crash by re-dispatching the
/// current state, so each transition writes state *before* its GitHub side
/// effect. Transitions:
///
/// ```text
/// Staging --staged--> Testing --green--> Merging --ff--> Merged*
///                        |  \--red--> Bisecting --1 left--> Ejected*
///                        |                       \-->1----> Staging (smaller batch)
///                        \--base moved--> Superseded*
/// ```
/// (* = terminal)
#[typeshare]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum, EnumIter, DeriveActiveEnum,
)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "batch_state")]
pub enum BatchState {
    /// Assembling the staging branch (base + each PR applied).
    #[sea_orm(string_value = "staging")]
    Staging,
    /// Staging pushed; awaiting required checks.
    #[sea_orm(string_value = "testing")]
    Testing,
    /// Checks green; fast-forwarding base.
    #[sea_orm(string_value = "merging")]
    Merging,
    /// Checks red; narrowing to the culprit PR.
    #[sea_orm(string_value = "bisecting")]
    Bisecting,
    /// Terminal: landed on base.
    #[sea_orm(string_value = "merged")]
    Merged,
    /// Terminal: a culprit was ejected; the remainder was re-queued.
    #[sea_orm(string_value = "ejected")]
    Ejected,
    /// Terminal: base moved or the batch was abandoned; entries re-queued.
    #[sea_orm(string_value = "superseded")]
    Superseded,
}

impl BatchState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Ejected | Self::Superseded)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Testing => "testing",
            Self::Merging => "merging",
            Self::Bisecting => "bisecting",
            Self::Merged => "merged",
            Self::Ejected => "ejected",
            Self::Superseded => "superseded",
        }
    }

    /// Parse the Postgres `batch_state` text. `None` on an unknown value.
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "staging" => Self::Staging,
            "testing" => Self::Testing,
            "merging" => Self::Merging,
            "bisecting" => Self::Bisecting,
            "merged" => Self::Merged,
            "ejected" => Self::Ejected,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub queue_id: Uuid,
    /// Entries in this batch, in queue order.
    pub entry_ids: Vec<Uuid>,
    /// Base tip captured when the batch was staged — if base moves past this, re-stage.
    pub base_sha: String,
    /// Tip of the staging branch once assembled.
    pub staging_sha: Option<String>,
    /// e.g. `mq/staging/main`.
    pub staging_ref: String,
    pub state: BatchState,
    pub created_at: DateTime<Utc>,
}

#[typeshare]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum, EnumIter, DeriveActiveEnum,
)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "merge_method")]
pub enum MergeMethod {
    #[sea_orm(string_value = "squash")]
    Squash,
    #[sea_orm(string_value = "merge")]
    Merge,
    #[sea_orm(string_value = "rebase")]
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }

    /// Parse the Postgres `merge_method` text. `None` on an unknown value.
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "squash" => Self::Squash,
            "merge" => Self::Merge,
            "rebase" => Self::Rebase,
            _ => return None,
        })
    }
}

/// Terminal outcome recorded in the append-only `queue_ledger` table.
#[typeshare]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum, EnumIter, DeriveActiveEnum,
)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "ledger_outcome")]
pub enum LedgerOutcome {
    /// All PRs in the batch landed on the base branch.
    #[sea_orm(string_value = "merged")]
    Merged,
    /// A culprit PR was ejected; the remainder was re-queued.
    #[sea_orm(string_value = "ejected")]
    Ejected,
    /// The batch was abandoned because the base moved or it was overtaken.
    #[sea_orm(string_value = "superseded")]
    Superseded,
}

/// Per-queue settings (persisted; editable from the UI). A repo may host several
/// named queues, each with its own config and staging refs.
#[derive(Debug, Clone)]
pub struct RepoQueueConfig {
    pub queue_id: Uuid,
    /// The repo the queue belongs to — carries GitHub identity for ref/API calls.
    pub repo_id: Uuid,
    /// Queue name; ref-safe and folded into the staging ref so queues never collide.
    pub name: String,
    /// Target branch, e.g. `main`.
    pub base_branch: String,
    /// Max PRs per batch. Start at 1 to validate the loop, then raise.
    pub batch_size: usize,
    /// Check-run / status contexts that must be green on the staging branch.
    pub required_checks: Vec<String>,
    pub merge_method: MergeMethod,
    /// Staging branch prefix; full ref is `<prefix>/<name>/<base_branch>`.
    pub staging_prefix: String,
}

impl RepoQueueConfig {
    /// The queue-scoped staging ref, `<prefix>/<name>/<base_branch>` — folding the
    /// queue name in keeps two queues on the same base from sharing a staging branch
    /// (and the `assembly_ref` derived from it stays CI-silent and collision-free).
    pub fn staging_ref(&self) -> String {
        format!(
            "{}/{}/{}",
            self.staging_prefix.trim_end_matches('/'),
            self.name,
            self.base_branch
        )
    }
}

/// What a single engine tick did — surfaced for logging/UI/tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    /// Nothing queued; nothing to do.
    Idle,
    /// The repo has no required checks configured, so the queue is held: the
    /// engine refuses to stage/land anything (it would otherwise fast-forward base
    /// with nothing gating it). Configure required checks to unblock.
    BlockedNoChecks,
    /// A new batch was staged and is now testing.
    Staged { batch: Uuid },
    /// Still waiting on required checks.
    Waiting,
    /// A batch landed; these PRs merged.
    Merged { prs: Vec<u64> },
    /// Base moved; staging was rebuilt.
    Restaged,
    /// A PR was ejected as the batch breaker.
    Ejected { pr: u64 },
}
