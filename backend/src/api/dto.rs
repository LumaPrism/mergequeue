//! The DTOs the dashboard consumes, with their projections from the domain
//! types. Every type carries the `#[typeshare]`/`#[oai]`/`#[serde]` attributes
//! the generated TypeScript and OpenAPI spec depend on, so the wire shape is
//! owned here in one place.

use poem_openapi::{Enum, Object};
use serde::{Deserialize, Serialize};
use typeshare::typeshare;

use crate::github::PullSummary;
use crate::queue::{
    BatchState, BatchView, EntryState, LedgerEntryResult, LedgerOutcome, QueueEntry,
};
use crate::store::{QueueSummary, RepoSummary, queue_ledger};

#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct Health {
    pub status: String,
}

#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct EntryView {
    pub id: String,
    pub pr_number: u32,
    pub position: i32,
    pub status: PrStatus,
}

/// A PR's place in the merge lifecycle — projected from its entry + batch.
#[typeshare]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Enum)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
pub enum PrStatus {
    Queued,
    Testing,
    Merging,
    Blocked,
    Merged,
    Ejected,
}

impl PrStatus {
    fn of(state: EntryState, batch: Option<&BatchView>) -> Self {
        match state {
            EntryState::Queued => Self::Queued,
            EntryState::Merged => Self::Merged,
            EntryState::Ejected => Self::Ejected,
            EntryState::Testing => match batch {
                Some(b) if b.merge_blocked => Self::Blocked,
                Some(b) if b.state == BatchState::Merging => Self::Merging,
                _ => Self::Testing,
            },
        }
    }
}

impl EntryView {
    pub fn project(e: QueueEntry, batch: Option<&BatchView>) -> Self {
        Self {
            status: PrStatus::of(e.state, batch),
            id: e.id.to_string(),
            pr_number: e.pr_number as u32,
            position: e.position,
        }
    }
}

/// One PR's fate within a finished batch run, projected from the ledger.
#[typeshare]
#[derive(Serialize, Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct LedgerEntryView {
    #[typeshare(serialized_as = "u32")]
    pub pr_number: u64,
    pub result: LedgerEntryResult,
}

/// One finished batch run from the append-only ledger (the dashboard's history view).
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct LedgerView {
    pub id: String,
    pub batch_id: String,
    pub outcome: LedgerOutcome,
    pub base_sha: String,
    pub landed_sha: Option<String>,
    #[typeshare(serialized_as = "Option<u32>")]
    pub ejected_pr: Option<u64>,
    pub entries: Vec<LedgerEntryView>,
    pub started_at: String,
    pub ended_at: String,
}

impl LedgerView {
    pub fn project(m: queue_ledger::Model) -> Self {
        Self {
            id: m.id.to_string(),
            batch_id: m.batch_id.to_string(),
            outcome: m.outcome,
            base_sha: m.base_sha,
            landed_sha: m.landed_sha,
            ejected_pr: m.ejected_pr.map(|p| p as u64),
            entries: serde_json::from_value(m.entries).unwrap_or_default(),
            started_at: m.started_at.to_rfc3339(),
            ended_at: m.ended_at.to_rfc3339(),
        }
    }
}

/// A repo under management, with its named queues (the dashboard's switcher).
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct RepoView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub queues: Vec<QueueView>,
}

impl From<RepoSummary> for RepoView {
    fn from(r: RepoSummary) -> Self {
        Self {
            id: r.id.to_string(),
            owner: r.owner,
            name: r.name,
            queues: r.queues.into_iter().map(QueueView::from).collect(),
        }
    }
}

/// One named queue with its config + live depth. `active` is the active-batch
/// summary; it's populated by the per-repo queues endpoint and left `None` in the
/// lightweight repo switcher.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct QueueView {
    pub id: String,
    pub repo_id: String,
    pub name: String,
    pub base_branch: String,
    pub batch_size: i32,
    pub depth: i32,
    pub active: Option<ActiveBatchView>,
}

impl From<QueueSummary> for QueueView {
    fn from(q: QueueSummary) -> Self {
        Self {
            id: q.id.to_string(),
            repo_id: q.repo_id.to_string(),
            name: q.name,
            base_branch: q.base_branch,
            batch_size: q.batch_size,
            depth: q.queued as i32,
            active: None,
        }
    }
}

/// A compact summary of a queue's in-flight batch.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct ActiveBatchView {
    pub id: String,
    pub state: BatchState,
    #[typeshare(serialized_as = "Vec<u32>")]
    pub prs: Vec<u64>,
}

impl From<&BatchView> for ActiveBatchView {
    fn from(batch: &BatchView) -> Self {
        Self {
            id: batch.id.to_string(),
            state: batch.state,
            prs: batch.prs(),
        }
    }
}

/// Create a queue: a name plus optional config overriding the repo's default queue.
#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct CreateQueueRequest {
    pub name: String,
    pub base_branch: Option<String>,
    pub batch_size: Option<i32>,
}

#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct EnqueueRequest {
    pub pr_number: u32,
}

/// Drag-to-reorder: the queued entry ids in their new order (front of the train first).
#[typeshare]
#[derive(Deserialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct ReorderRequest {
    pub entry_ids: Vec<String>,
}

/// The signed-in GitHub user, for the dashboard's auth gate.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct MeView {
    pub login: String,
    pub avatar_url: String,
}

/// An open PR — a candidate to add to the queue.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct PrView {
    pub number: u32,
    pub title: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: Option<bool>,
}

impl From<PullSummary> for PrView {
    fn from(p: PullSummary) -> Self {
        Self {
            number: p.number as u32,
            title: p.title,
            head_ref: p.head_ref,
            base_ref: p.base_ref,
            mergeable: p.mergeable,
        }
    }
}

/// Where the resolved GitHub App credentials came from.
#[typeshare]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Enum)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")]
pub enum SetupSource {
    /// Static credentials from config/env (the static escape hatch).
    Config,
    /// Credentials minted by the `/setup` manifest flow, stored in the DB.
    Manifest,
}

/// Whether the GitHub App is registered. Drives the dashboard's setup gate so the
/// manifest flow is offered only when the App is missing.
#[typeshare]
#[derive(Serialize, Object)]
#[serde(rename_all = "camelCase")]
#[oai(rename_all = "camelCase")]
pub struct SetupStatus {
    pub registered: bool,
    /// Where to start the manifest flow (always present, used by the "connect" CTA).
    pub setup_url: String,
    pub slug: Option<String>,
    pub install_url: Option<String>,
    pub manage_url: Option<String>,
    pub source: Option<SetupSource>,
}
