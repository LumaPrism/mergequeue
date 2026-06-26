//! The pure core of the engine. `State::decide` is a total, side-effect-free
//! function from an `Observation` (the live facts gathered by `Engine::observe`)
//! to a `Decision` (an ordered list of `Effect`s plus a control-flow signal). The
//! engine's `interpret` step is the only place that touches GitHub or the DB, so
//! the whole transition table here is unit-testable without either.
//!
//! Effect ordering is load-bearing for crash-resume: the state-defining `Db`
//! write is placed where a crash leaves a resumable state, and every `Gh` effect
//! is idempotent on replay. In particular `eject`/`supersede`/`finalize` commit
//! the batch's terminal state *before* the best-effort `DeleteRef`, so a crash in
//! between never leaves a live batch pointing at a deleted staging branch.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::model::{BatchState, EntryState, RepoQueueConfig, TickOutcome};
use crate::github::{CheckState, MergeOutcome, TrainLabel};

/// The commit-status context the pre-labels version posted on PR heads. It is not
/// CI, so it never counts as "a real check reported".
const LEGACY_BADGE_CONTEXT: &str = "mergequeue";

/// The complete fact set `decide` needs for one step, produced by `observe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// `required_checks` is empty — the no-checks safety guard fired before any IO.
    Blocked,
    /// No active batch; the next `batch_size` queued entry ids (possibly empty).
    Empty { queued: Vec<Uuid> },
    /// An active batch plus the live fact for its current state.
    Active { batch: BatchView, fact: Fact },
}

/// The persisted batch projected for decisions, loaded fresh every loop step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchView {
    pub id: Uuid,
    pub state: BatchState,
    /// Base tip captured at the reset step; empty until then (race guard inactive).
    pub base_sha: String,
    pub staging_sha: Option<String>,
    pub staging_ref: String,
    /// True once the base fast-forward was rejected and the operator was told —
    /// suppresses re-commenting while the engine keeps retrying the merge.
    pub merge_blocked: bool,
    /// Entries in queue (`ord`) order, with per-PR staging progress.
    pub entries: Vec<EntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryView {
    pub entry_id: Uuid,
    pub pr_number: u64,
    /// True once this PR's head is durably merged onto the staging branch.
    pub staged: bool,
}

impl BatchView {
    pub fn entry_ids(&self) -> Vec<Uuid> {
        self.entries.iter().map(|e| e.entry_id).collect()
    }

    pub fn prs(&self) -> Vec<u64> {
        self.entries.iter().map(|e| e.pr_number).collect()
    }

    /// The lowest-`ord` entry whose head isn't on staging yet.
    pub fn next_unstaged(&self) -> Option<&EntryView> {
        self.entries.iter().find(|e| !e.staged)
    }
}

/// The result of a `MergeOnto` effect, threaded from `interpret` into the next
/// `observe` within the SAME tick. Tick-local; never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub entry_id: Uuid,
    pub pr_number: u64,
    pub outcome: MergeOutcome,
}

/// What a result-bearing `Gh` effect reported back, threaded into the next
/// `observe` within the SAME tick. Tick-local; never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepReport {
    /// A `MergeOnto` verdict.
    Merged(MergeReport),
    /// A `FastForward` of the base branch was rejected (branch protection /
    /// not-a-fast-forward).
    FfRejected,
}

/// The live-world fact for an active batch's current decision point. `observe`
/// produces exactly the variant the batch's state calls for; `decide` matches it
/// exhaustively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    /// Race guard: base advanced past `batch.base_sha` (Staging/Testing only).
    BaseMoved,
    /// `base_sha` is empty → reset staging to the current base tip.
    StageReset { base: String },
    /// Lowest-`ord` unstaged entry, resolved live off GitHub.
    StageNext {
        entry_id: Uuid,
        pr_number: u64,
        head_sha: String,
        base_ref: String,
    },
    /// The merge just attempted for this entry came back (round-tripped verdict).
    StageMerged {
        entry_id: Uuid,
        pr_number: u64,
        outcome: MergeOutcome,
    },
    /// Every entry is staged; here is the assembled staging tip.
    StageFinalize { staging_sha: String },
    /// `None` = no real CI reported yet → hold; `Some` = the applicable verdict.
    Checks { verdict: Option<CheckState> },
    /// Merging: current base tip drives the 3-way idempotent resume (no race guard).
    MergeBase { current: String },
    /// Merging: the base fast-forward was just rejected and base hasn't moved —
    /// a genuine block (vs. a race), so tell the operator how to unblock it.
    FfRejected,
    /// Bisecting: roster comes off `batch.entries`.
    Bisect,
}

/// One side effect. A `Db` group runs in ONE transaction; a `Gh` call runs singly.
/// `decide` emits them in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Db(Vec<DbWrite>),
    Gh(GhCall),
}

/// A persisted mutation; each maps 1:1 onto a `Store` associated function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbWrite {
    CreateBatch {
        entry_ids: Vec<Uuid>,
        staging_ref: String,
    },
    SetBatchBaseSha {
        batch_id: Uuid,
        base_sha: String,
    },
    MarkEntryStaged {
        batch_id: Uuid,
        entry_id: Uuid,
    },
    /// Records the staging tip and flips the batch to `Testing`.
    SetBatchStaged {
        batch_id: Uuid,
        staging_sha: String,
    },
    SetBatchState {
        batch_id: Uuid,
        state: BatchState,
    },
    SetMergeBlocked {
        batch_id: Uuid,
        blocked: bool,
    },
    SetEntriesState {
        entry_ids: Vec<Uuid>,
        state: EntryState,
    },
    RequeueEntries {
        entry_ids: Vec<Uuid>,
    },
}

/// A GitHub mutation; each maps 1:1 onto a `RepoClient` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhCall {
    ForceRef {
        staging_ref: String,
        sha: String,
    },
    /// The one result-bearing effect: `interpret` returns its `MergeReport`. It
    /// writes nothing itself — `decide` persists the progress next iteration.
    MergeOnto {
        staging_ref: String,
        head: String,
        message: String,
        entry_id: Uuid,
        pr_number: u64,
    },
    FastForward {
        base_branch: String,
        sha: String,
    },
    DeleteRef {
        staging_ref: String,
    },
    Comment {
        pr: u64,
        body: String,
    },
    SetLabel {
        pr: u64,
        target: Option<TrainLabel>,
    },
}

/// The outcome of one `decide` step: effects to run, and whether the tick keeps
/// looping (`Continue`) or finishes (`Done`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub effects: Vec<Effect>,
    pub flow: Flow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Done(TickOutcome),
}

/// The pure FSM. Zero-sized; behaviour on associated functions.
pub struct State;

impl State {
    /// `None` ⇒ no real CI reported yet → hold. `Some(set)` ⇒ gate on exactly
    /// these contexts (empty when a PR legitimately triggers none → it merges).
    pub fn applicable_checks(
        required: &[String],
        reported_per_pr: &[Vec<String>],
    ) -> Option<Vec<String>> {
        let mut applicable: BTreeSet<String> = BTreeSet::new();
        let mut any = false;
        for reported in reported_per_pr {
            for ctx in reported {
                if ctx == LEGACY_BADGE_CONTEXT {
                    continue;
                }
                any = true;
                if required.iter().any(|r| r == ctx) {
                    applicable.insert(ctx.clone());
                }
            }
        }
        if any {
            Some(applicable.into_iter().collect())
        } else {
            None
        }
    }

    /// The total, side-effect-free transition function.
    pub fn decide(cfg: &RepoQueueConfig, obs: &Observation) -> Decision {
        match obs {
            Observation::Blocked => Self::done(TickOutcome::BlockedNoChecks),
            Observation::Empty { queued } if queued.is_empty() => Self::done(TickOutcome::Idle),
            Observation::Empty { queued } => Decision {
                effects: vec![Effect::Db(vec![
                    DbWrite::CreateBatch {
                        entry_ids: queued.clone(),
                        staging_ref: cfg.staging_ref(),
                    },
                    DbWrite::SetEntriesState {
                        entry_ids: queued.clone(),
                        state: EntryState::Testing,
                    },
                ])],
                flow: Flow::Continue,
            },
            Observation::Active { batch, fact } => Self::decide_active(cfg, batch, fact),
        }
    }

    fn decide_active(cfg: &RepoQueueConfig, batch: &BatchView, fact: &Fact) -> Decision {
        match fact {
            Fact::BaseMoved => Decision {
                effects: Self::supersede(batch),
                flow: Flow::Done(TickOutcome::Restaged),
            },
            Fact::StageReset { base } => Decision {
                effects: vec![
                    Effect::Gh(GhCall::ForceRef {
                        staging_ref: batch.staging_ref.clone(),
                        sha: base.clone(),
                    }),
                    Effect::Db(vec![DbWrite::SetBatchBaseSha {
                        batch_id: batch.id,
                        base_sha: base.clone(),
                    }]),
                ],
                flow: Flow::Continue,
            },
            Fact::StageNext {
                entry_id,
                pr_number,
                head_sha,
                base_ref,
            } => {
                if base_ref != &cfg.base_branch {
                    let reason = format!("retargeted off `{}`", cfg.base_branch);
                    Decision {
                        effects: Self::eject(batch, *entry_id, *pr_number, &reason),
                        flow: Flow::Done(TickOutcome::Ejected { pr: *pr_number }),
                    }
                } else {
                    let message = format!("mq: merge #{pr_number} into {}", batch.staging_ref);
                    Decision {
                        effects: vec![Effect::Gh(GhCall::MergeOnto {
                            staging_ref: batch.staging_ref.clone(),
                            head: head_sha.clone(),
                            message,
                            entry_id: *entry_id,
                            pr_number: *pr_number,
                        })],
                        flow: Flow::Continue,
                    }
                }
            }
            Fact::StageMerged {
                entry_id,
                pr_number,
                outcome,
            } => match outcome {
                MergeOutcome::Merged => Decision {
                    effects: vec![Effect::Db(vec![DbWrite::MarkEntryStaged {
                        batch_id: batch.id,
                        entry_id: *entry_id,
                    }])],
                    flow: Flow::Continue,
                },
                MergeOutcome::Conflicted => {
                    let reason = format!("merge conflicts with `{}`", cfg.base_branch);
                    Decision {
                        effects: Self::eject(batch, *entry_id, *pr_number, &reason),
                        flow: Flow::Done(TickOutcome::Ejected { pr: *pr_number }),
                    }
                }
            },
            Fact::StageFinalize { staging_sha } => {
                let mut effects = vec![Effect::Db(vec![DbWrite::SetBatchStaged {
                    batch_id: batch.id,
                    staging_sha: staging_sha.clone(),
                }])];
                for e in &batch.entries {
                    effects.push(Effect::Gh(GhCall::SetLabel {
                        pr: e.pr_number,
                        target: Some(TrainLabel::Testing),
                    }));
                }
                Decision {
                    effects,
                    flow: Flow::Done(TickOutcome::Staged { batch: batch.id }),
                }
            }
            Fact::Checks { verdict } => match verdict {
                None | Some(CheckState::Pending) => Self::done(TickOutcome::Waiting),
                Some(CheckState::Success) => Decision {
                    effects: vec![Effect::Db(vec![DbWrite::SetBatchState {
                        batch_id: batch.id,
                        state: BatchState::Merging,
                    }])],
                    flow: Flow::Continue,
                },
                Some(CheckState::Failure) => Decision {
                    effects: vec![Effect::Db(vec![DbWrite::SetBatchState {
                        batch_id: batch.id,
                        state: BatchState::Bisecting,
                    }])],
                    flow: Flow::Continue,
                },
            },
            Fact::MergeBase { current } => {
                let staging_sha = batch.staging_sha.clone().unwrap_or_default();
                if current == &staging_sha {
                    Decision {
                        effects: Self::finalize_merge(batch),
                        flow: Flow::Done(TickOutcome::Merged { prs: batch.prs() }),
                    }
                } else if current == &batch.base_sha {
                    Decision {
                        effects: vec![Effect::Gh(GhCall::FastForward {
                            base_branch: cfg.base_branch.clone(),
                            sha: staging_sha,
                        })],
                        flow: Flow::Continue,
                    }
                } else {
                    Decision {
                        effects: Self::supersede(batch),
                        flow: Flow::Done(TickOutcome::Restaged),
                    }
                }
            }
            Fact::FfRejected => Self::ff_rejected(cfg, batch),
            Fact::Bisect => Self::decide_bisect(cfg, batch),
        }
    }

    fn decide_bisect(cfg: &RepoQueueConfig, batch: &BatchView) -> Decision {
        if batch.entries.len() == 1 {
            let e = &batch.entries[0];
            let effects = vec![
                Effect::Db(vec![
                    DbWrite::SetEntriesState {
                        entry_ids: vec![e.entry_id],
                        state: EntryState::Ejected,
                    },
                    DbWrite::SetBatchState {
                        batch_id: batch.id,
                        state: BatchState::Ejected,
                    },
                ]),
                Effect::Gh(GhCall::DeleteRef {
                    staging_ref: batch.staging_ref.clone(),
                }),
                Effect::Gh(GhCall::Comment {
                    pr: e.pr_number,
                    body: "**mergequeue** · ejected · failed required checks".to_string(),
                }),
                Effect::Gh(GhCall::SetLabel {
                    pr: e.pr_number,
                    target: Some(TrainLabel::Ejected),
                }),
            ];
            return Decision {
                effects,
                flow: Flow::Done(TickOutcome::Ejected { pr: e.pr_number }),
            };
        }

        let mid = batch.entries.len().div_ceil(2);
        let first: Vec<Uuid> = batch.entries[..mid].iter().map(|e| e.entry_id).collect();
        let rest: Vec<Uuid> = batch.entries[mid..].iter().map(|e| e.entry_id).collect();
        let rest_prs: Vec<u64> = batch.entries[mid..].iter().map(|e| e.pr_number).collect();
        let mut effects = vec![Effect::Db(vec![
            DbWrite::RequeueEntries {
                entry_ids: rest.clone(),
            },
            DbWrite::SetBatchState {
                batch_id: batch.id,
                state: BatchState::Superseded,
            },
            DbWrite::CreateBatch {
                entry_ids: first.clone(),
                staging_ref: cfg.staging_ref(),
            },
            DbWrite::SetEntriesState {
                entry_ids: first,
                state: EntryState::Testing,
            },
        ])];
        effects.push(Effect::Gh(GhCall::DeleteRef {
            staging_ref: batch.staging_ref.clone(),
        }));
        for pr in rest_prs {
            effects.push(Effect::Gh(GhCall::SetLabel {
                pr,
                target: Some(TrainLabel::Queued),
            }));
        }
        Decision {
            effects,
            flow: Flow::Done(TickOutcome::Restaged),
        }
    }

    /// Re-queue every entry and drop the batch — base moved or the batch was
    /// abandoned. DB state first, then the best-effort staging-ref delete.
    fn supersede(batch: &BatchView) -> Vec<Effect> {
        let mut fx = vec![Effect::Db(vec![
            DbWrite::RequeueEntries {
                entry_ids: batch.entry_ids(),
            },
            DbWrite::SetBatchState {
                batch_id: batch.id,
                state: BatchState::Superseded,
            },
        ])];
        fx.push(Effect::Gh(GhCall::DeleteRef {
            staging_ref: batch.staging_ref.clone(),
        }));
        for e in &batch.entries {
            fx.push(Effect::Gh(GhCall::SetLabel {
                pr: e.pr_number,
                target: Some(TrainLabel::Queued),
            }));
        }
        fx
    }

    /// Eject one entry, re-queue the rest, supersede the batch. DB state first,
    /// then the best-effort delete/comment/label — so a crash before the commit
    /// leaves the staging ref intact and the entry re-merges→409→re-ejects.
    fn eject(batch: &BatchView, entry_id: Uuid, pr_number: u64, reason: &str) -> Vec<Effect> {
        let others: Vec<Uuid> = batch
            .entries
            .iter()
            .map(|e| e.entry_id)
            .filter(|id| *id != entry_id)
            .collect();
        let other_prs: Vec<u64> = batch
            .entries
            .iter()
            .filter(|e| e.entry_id != entry_id)
            .map(|e| e.pr_number)
            .collect();
        let mut fx = vec![Effect::Db(vec![
            DbWrite::SetEntriesState {
                entry_ids: vec![entry_id],
                state: EntryState::Ejected,
            },
            DbWrite::RequeueEntries { entry_ids: others },
            DbWrite::SetBatchState {
                batch_id: batch.id,
                state: BatchState::Superseded,
            },
        ])];
        fx.push(Effect::Gh(GhCall::DeleteRef {
            staging_ref: batch.staging_ref.clone(),
        }));
        fx.push(Effect::Gh(GhCall::Comment {
            pr: pr_number,
            body: format!("**mergequeue** · ejected · {reason}"),
        }));
        fx.push(Effect::Gh(GhCall::SetLabel {
            pr: pr_number,
            target: Some(TrainLabel::Ejected),
        }));
        for pr in other_prs {
            fx.push(Effect::Gh(GhCall::SetLabel {
                pr,
                target: Some(TrainLabel::Queued),
            }));
        }
        fx
    }

    /// Land the batch: mark entries/batch Merged, then drop staging and clear the
    /// train labels. DB state first, so a resume sees Merged and never re-merges.
    fn finalize_merge(batch: &BatchView) -> Vec<Effect> {
        let mut fx = vec![Effect::Db(vec![
            DbWrite::SetEntriesState {
                entry_ids: batch.entry_ids(),
                state: EntryState::Merged,
            },
            DbWrite::SetBatchState {
                batch_id: batch.id,
                state: BatchState::Merged,
            },
        ])];
        fx.push(Effect::Gh(GhCall::DeleteRef {
            staging_ref: batch.staging_ref.clone(),
        }));
        for pr in batch.prs() {
            fx.push(Effect::Gh(GhCall::SetLabel { pr, target: None }));
        }
        fx
    }

    /// The base fast-forward was rejected with base unchanged — a genuine block.
    /// Tell the operator once (comment + `Blocked` label per PR) and hold; the
    /// batch stays `Merging` so it lands the moment the block is lifted.
    fn ff_rejected(cfg: &RepoQueueConfig, batch: &BatchView) -> Decision {
        if batch.merge_blocked {
            return Self::done(TickOutcome::Waiting);
        }
        let body = format!(
            "**mergequeue** · blocked · CI passed, but `{base}`'s branch protection rejected the \
             fast-forward. Add the mergequeue GitHub App as a bypass actor on the `{base}` ruleset \
             and it will land on the next check.",
            base = cfg.base_branch
        );
        let mut effects = vec![Effect::Db(vec![DbWrite::SetMergeBlocked {
            batch_id: batch.id,
            blocked: true,
        }])];
        for pr in batch.prs() {
            effects.push(Effect::Gh(GhCall::Comment {
                pr,
                body: body.clone(),
            }));
            effects.push(Effect::Gh(GhCall::SetLabel {
                pr,
                target: Some(TrainLabel::Blocked),
            }));
        }
        Decision {
            effects,
            flow: Flow::Done(TickOutcome::Waiting),
        }
    }

    fn done(outcome: TickOutcome) -> Decision {
        Decision {
            effects: vec![],
            flow: Flow::Done(outcome),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::MergeMethod;

    fn cfg() -> RepoQueueConfig {
        RepoQueueConfig {
            repo_id: Uuid::nil(),
            base_branch: "main".into(),
            batch_size: 2,
            required_checks: vec!["ci".into()],
            merge_method: MergeMethod::Squash,
            staging_prefix: "mq/staging".into(),
        }
    }

    fn entry(pr: u64, staged: bool) -> EntryView {
        EntryView {
            entry_id: Uuid::from_u128(pr as u128),
            pr_number: pr,
            staged,
        }
    }

    fn batch(
        state: BatchState,
        base_sha: &str,
        staging_sha: Option<&str>,
        entries: Vec<EntryView>,
    ) -> BatchView {
        BatchView {
            id: Uuid::from_u128(9000),
            state,
            base_sha: base_sha.into(),
            staging_sha: staging_sha.map(str::to_owned),
            staging_ref: "mq/staging/main".into(),
            merge_blocked: false,
            entries,
        }
    }

    fn active(batch: BatchView, fact: Fact) -> Observation {
        Observation::Active { batch, fact }
    }

    fn idx(effects: &[Effect], pred: impl Fn(&Effect) -> bool) -> Option<usize> {
        effects.iter().position(pred)
    }

    #[test]
    fn test_state_blocked_holds() {
        let d = State::decide(&cfg(), &Observation::Blocked);
        assert_eq!(d, State::done(TickOutcome::BlockedNoChecks));
    }

    #[test]
    fn test_state_empty_queue_is_idle() {
        let d = State::decide(&cfg(), &Observation::Empty { queued: vec![] });
        assert_eq!(d.flow, Flow::Done(TickOutcome::Idle));
        assert!(d.effects.is_empty());
    }

    #[test]
    fn test_state_empty_with_queued_creates_batch() {
        let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        let d = State::decide(
            &cfg(),
            &Observation::Empty {
                queued: ids.clone(),
            },
        );
        assert_eq!(d.flow, Flow::Continue);
        assert_eq!(
            d.effects,
            vec![Effect::Db(vec![
                DbWrite::CreateBatch {
                    entry_ids: ids.clone(),
                    staging_ref: "mq/staging/main".into()
                },
                DbWrite::SetEntriesState {
                    entry_ids: ids,
                    state: EntryState::Testing
                },
            ])]
        );
    }

    #[test]
    fn test_state_stage_reset_forces_then_persists_base() {
        let b = batch(BatchState::Staging, "", None, vec![entry(1, false)]);
        let d = State::decide(
            &cfg(),
            &active(
                b,
                Fact::StageReset {
                    base: "base9".into(),
                },
            ),
        );
        assert_eq!(d.flow, Flow::Continue);
        assert_eq!(
            d.effects,
            vec![
                Effect::Gh(GhCall::ForceRef {
                    staging_ref: "mq/staging/main".into(),
                    sha: "base9".into()
                }),
                Effect::Db(vec![DbWrite::SetBatchBaseSha {
                    batch_id: Uuid::from_u128(9000),
                    base_sha: "base9".into()
                }]),
            ]
        );
    }

    #[test]
    fn test_state_stage_next_merges() {
        let b = batch(BatchState::Staging, "base9", None, vec![entry(7, false)]);
        let fact = Fact::StageNext {
            entry_id: Uuid::from_u128(7),
            pr_number: 7,
            head_sha: "h7".into(),
            base_ref: "main".into(),
        };
        let d = State::decide(&cfg(), &active(b, fact));
        assert_eq!(d.flow, Flow::Continue);
        assert_eq!(
            d.effects,
            vec![Effect::Gh(GhCall::MergeOnto {
                staging_ref: "mq/staging/main".into(),
                head: "h7".into(),
                message: "mq: merge #7 into mq/staging/main".into(),
                entry_id: Uuid::from_u128(7),
                pr_number: 7,
            })]
        );
    }

    #[test]
    fn test_state_stage_next_retargeted_ejects_db_before_deleteref() {
        let b = batch(BatchState::Staging, "base9", None, vec![entry(7, false)]);
        let fact = Fact::StageNext {
            entry_id: Uuid::from_u128(7),
            pr_number: 7,
            head_sha: "h7".into(),
            base_ref: "release".into(),
        };
        let d = State::decide(&cfg(), &active(b, fact));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Ejected { pr: 7 }));
        let db = idx(&d.effects, |e| matches!(e, Effect::Db(_))).unwrap();
        let del = idx(&d.effects, |e| {
            matches!(e, Effect::Gh(GhCall::DeleteRef { .. }))
        })
        .unwrap();
        assert!(
            db < del,
            "eject must commit DB state before deleting the staging ref"
        );
    }

    #[test]
    fn test_state_stage_merged_marks_staged() {
        let b = batch(BatchState::Staging, "base9", None, vec![entry(7, false)]);
        let fact = Fact::StageMerged {
            entry_id: Uuid::from_u128(7),
            pr_number: 7,
            outcome: MergeOutcome::Merged,
        };
        let d = State::decide(&cfg(), &active(b, fact));
        assert_eq!(
            d,
            Decision {
                effects: vec![Effect::Db(vec![DbWrite::MarkEntryStaged {
                    batch_id: Uuid::from_u128(9000),
                    entry_id: Uuid::from_u128(7)
                }])],
                flow: Flow::Continue,
            }
        );
    }

    #[test]
    fn test_state_stage_merged_conflict_ejects() {
        let b = batch(BatchState::Staging, "base9", None, vec![entry(7, false)]);
        let fact = Fact::StageMerged {
            entry_id: Uuid::from_u128(7),
            pr_number: 7,
            outcome: MergeOutcome::Conflicted,
        };
        let d = State::decide(&cfg(), &active(b, fact));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Ejected { pr: 7 }));
    }

    #[test]
    fn test_state_stage_finalize_flips_testing_and_labels() {
        let b = batch(
            BatchState::Staging,
            "base9",
            None,
            vec![entry(7, true), entry(8, true)],
        );
        let d = State::decide(
            &cfg(),
            &active(
                b,
                Fact::StageFinalize {
                    staging_sha: "stg".into(),
                },
            ),
        );
        assert_eq!(
            d.flow,
            Flow::Done(TickOutcome::Staged {
                batch: Uuid::from_u128(9000)
            })
        );
        assert_eq!(
            d.effects[0],
            Effect::Db(vec![DbWrite::SetBatchStaged {
                batch_id: Uuid::from_u128(9000),
                staging_sha: "stg".into()
            }])
        );
        assert!(d.effects.contains(&Effect::Gh(GhCall::SetLabel {
            pr: 7,
            target: Some(TrainLabel::Testing)
        })));
        assert!(d.effects.contains(&Effect::Gh(GhCall::SetLabel {
            pr: 8,
            target: Some(TrainLabel::Testing)
        })));
    }

    #[test]
    fn test_state_checks_none_and_pending_wait() {
        let b = batch(
            BatchState::Testing,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        assert_eq!(
            State::decide(&cfg(), &active(b.clone(), Fact::Checks { verdict: None })).flow,
            Flow::Done(TickOutcome::Waiting)
        );
        assert_eq!(
            State::decide(
                &cfg(),
                &active(
                    b,
                    Fact::Checks {
                        verdict: Some(CheckState::Pending)
                    }
                )
            )
            .flow,
            Flow::Done(TickOutcome::Waiting)
        );
    }

    #[test]
    fn test_state_checks_success_to_merging_failure_to_bisecting() {
        let b = batch(
            BatchState::Testing,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let s = State::decide(
            &cfg(),
            &active(
                b.clone(),
                Fact::Checks {
                    verdict: Some(CheckState::Success),
                },
            ),
        );
        assert_eq!(
            s,
            Decision {
                effects: vec![Effect::Db(vec![DbWrite::SetBatchState {
                    batch_id: Uuid::from_u128(9000),
                    state: BatchState::Merging
                }])],
                flow: Flow::Continue
            }
        );
        let f = State::decide(
            &cfg(),
            &active(
                b,
                Fact::Checks {
                    verdict: Some(CheckState::Failure),
                },
            ),
        );
        assert_eq!(
            f,
            Decision {
                effects: vec![Effect::Db(vec![DbWrite::SetBatchState {
                    batch_id: Uuid::from_u128(9000),
                    state: BatchState::Bisecting
                }])],
                flow: Flow::Continue
            }
        );
    }

    #[test]
    fn test_state_merging_already_landed_finalizes_without_ff() {
        let b = batch(
            BatchState::Merging,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(
            &cfg(),
            &active(
                b,
                Fact::MergeBase {
                    current: "stg".into(),
                },
            ),
        );
        assert_eq!(d.flow, Flow::Done(TickOutcome::Merged { prs: vec![7] }));
        assert!(
            idx(&d.effects, |e| matches!(
                e,
                Effect::Gh(GhCall::FastForward { .. })
            ))
            .is_none(),
            "already landed: no fast-forward"
        );
    }

    #[test]
    fn test_state_merging_unlanded_fast_forwards_and_continues() {
        let b = batch(
            BatchState::Merging,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(
            &cfg(),
            &active(
                b,
                Fact::MergeBase {
                    current: "base9".into(),
                },
            ),
        );
        assert_eq!(
            d,
            Decision {
                effects: vec![Effect::Gh(GhCall::FastForward {
                    base_branch: "main".into(),
                    sha: "stg".into()
                })],
                flow: Flow::Continue,
            }
        );
    }

    #[test]
    fn test_state_ff_rejected_first_time_comments_and_blocks() {
        let b = batch(
            BatchState::Merging,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(&cfg(), &active(b, Fact::FfRejected));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Waiting));
        assert_eq!(
            d.effects[0],
            Effect::Db(vec![DbWrite::SetMergeBlocked {
                batch_id: Uuid::from_u128(9000),
                blocked: true,
            }])
        );
        assert!(
            d.effects
                .iter()
                .any(|e| matches!(e, Effect::Gh(GhCall::Comment { pr: 7, .. })))
        );
        assert!(d.effects.contains(&Effect::Gh(GhCall::SetLabel {
            pr: 7,
            target: Some(TrainLabel::Blocked),
        })));
    }

    #[test]
    fn test_state_ff_rejected_again_is_silent() {
        let mut b = batch(
            BatchState::Merging,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        b.merge_blocked = true;
        let d = State::decide(&cfg(), &active(b, Fact::FfRejected));
        assert_eq!(d, State::done(TickOutcome::Waiting));
    }

    #[test]
    fn test_state_merging_base_advanced_supersedes() {
        let b = batch(
            BatchState::Merging,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(
            &cfg(),
            &active(
                b,
                Fact::MergeBase {
                    current: "other".into(),
                },
            ),
        );
        assert_eq!(d.flow, Flow::Done(TickOutcome::Restaged));
        assert!(
            idx(&d.effects, |e| matches!(
                e,
                Effect::Gh(GhCall::FastForward { .. })
            ))
            .is_none()
        );
    }

    #[test]
    fn test_state_base_moved_supersedes_db_before_deleteref_and_relabels() {
        let b = batch(
            BatchState::Testing,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(&cfg(), &active(b, Fact::BaseMoved));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Restaged));
        let db = idx(&d.effects, |e| matches!(e, Effect::Db(_))).unwrap();
        let del = idx(&d.effects, |e| {
            matches!(e, Effect::Gh(GhCall::DeleteRef { .. }))
        })
        .unwrap();
        assert!(db < del);
        assert!(d.effects.contains(&Effect::Gh(GhCall::SetLabel {
            pr: 7,
            target: Some(TrainLabel::Queued)
        })));
    }

    #[test]
    fn test_state_bisect_one_ejects() {
        let b = batch(
            BatchState::Bisecting,
            "base9",
            Some("stg"),
            vec![entry(7, true)],
        );
        let d = State::decide(&cfg(), &active(b, Fact::Bisect));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Ejected { pr: 7 }));
        let db = idx(&d.effects, |e| matches!(e, Effect::Db(_))).unwrap();
        let del = idx(&d.effects, |e| {
            matches!(e, Effect::Gh(GhCall::DeleteRef { .. }))
        })
        .unwrap();
        assert!(db < del);
    }

    #[test]
    fn test_state_bisect_splits_div_ceil() {
        let b = batch(
            BatchState::Bisecting,
            "base9",
            Some("stg"),
            vec![entry(7, true), entry(8, true), entry(9, true)],
        );
        let d = State::decide(&cfg(), &active(b, Fact::Bisect));
        assert_eq!(d.flow, Flow::Done(TickOutcome::Restaged));
        let Effect::Db(writes) = &d.effects[0] else {
            panic!("first effect is the db group")
        };
        assert!(
            writes.contains(&DbWrite::RequeueEntries {
                entry_ids: vec![Uuid::from_u128(9)]
            }),
            "n=3 keeps first 2, requeues last 1"
        );
        assert!(writes.contains(&DbWrite::CreateBatch {
            entry_ids: vec![Uuid::from_u128(7), Uuid::from_u128(8)],
            staging_ref: "mq/staging/main".into()
        }));
    }

    #[test]
    fn test_state_applicable_legacy_badge_is_ignored() {
        assert_eq!(
            State::applicable_checks(&["ci".into()], &[vec!["mergequeue".into()]]),
            None
        );
    }

    #[test]
    fn test_state_applicable_nothing_reported_is_none() {
        assert_eq!(State::applicable_checks(&["ci".into()], &[vec![]]), None);
    }

    #[test]
    fn test_state_applicable_path_filtered_is_empty_some() {
        assert_eq!(
            State::applicable_checks(&["ci".into()], &[vec!["changes".into()]]),
            Some(vec![])
        );
    }

    #[test]
    fn test_state_applicable_intersects_required() {
        assert_eq!(
            State::applicable_checks(
                &["ci".into(), "lint".into()],
                &[vec!["ci".into(), "changes".into()]]
            ),
            Some(vec!["ci".into()])
        );
    }
}
