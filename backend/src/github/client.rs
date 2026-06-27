//! Repo operations the engine depends on. `RepoClient` is the seam: the engine
//! talks only to this trait, so tests inject a fake and never hit GitHub.

use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use octocrab::Octocrab;
use octocrab::models::checks::ListCheckRuns;
use octocrab::models::repos::Object;
use octocrab::models::{CombinedStatus, StatusState};
use octocrab::params::repos::Reference;
use serde::Deserialize;

use super::GitHubError;

/// `owner/name` plus the installation that grants access to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    pub owner: String,
    pub name: String,
    pub installation_id: u64,
}

#[derive(Debug, Clone)]
pub struct PullSummary {
    pub number: u64,
    pub title: String,
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: Option<bool>,
    pub approved: bool,
}

/// Aggregate of the repo's *required* checks at a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    /// At least one required check has not reported a terminal state yet.
    Pending,
    /// Every required check reported success (skipped counts as success).
    Success,
    /// At least one required check failed/errored/was cancelled.
    Failure,
}

/// What happened when merging a PR head onto the staging branch. All the states
/// the engine acts on; any other failure surfaces as a `GitHubError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Merged onto staging (or already contained) — staging is up to date with the PR.
    Merged,
    /// The PR conflicts with the staging branch and can't be merged.
    Conflicted,
}

/// The PR labels mergequeue manages to show train state at a glance. They're
/// mutually exclusive — a PR carries at most one — and are cleared when the PR
/// leaves the train (merged, ejected, or manually dequeued).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainLabel {
    Queued,
    Testing,
    Blocked,
    Ejected,
}

impl TrainLabel {
    /// Every managed label — used to clear the others when state changes.
    pub const ALL: [TrainLabel; 4] = [Self::Queued, Self::Testing, Self::Blocked, Self::Ejected];

    /// The label text as it appears on the PR, scoped to its queue:
    /// `merge-queue (<queue>): <state>`. Every queue gets its own label variants so a
    /// PR's label says which named queue it's on.
    pub fn name(self, queue: &str) -> String {
        let state = match self {
            Self::Queued => "queued",
            Self::Testing => "testing",
            Self::Blocked => "blocked",
            Self::Ejected => "ejected",
        };
        format!("merge-queue ({queue}): {state}")
    }

    /// The label colour (GitHub hex, no leading `#`).
    fn color(self) -> &'static str {
        match self {
            Self::Queued => "1f6feb",
            Self::Testing => "d29922",
            Self::Blocked => "bf8700",
            Self::Ejected => "cf222e",
        }
    }

    /// The label description shown in the repo's label list.
    fn description(self) -> &'static str {
        match self {
            Self::Queued => "Waiting in the merge queue",
            Self::Testing => "Being tested in the merge train",
            Self::Blocked => "Can't land — base-branch protection rejected the merge",
            Self::Ejected => "Removed from the merge train — see the PR comment",
        }
    }
}

/// A user's permission level on a repo, from the collaborator-permission API.
/// Used to authorize merge-queue commands posted as PR comments — the comment
/// payload's `author_association` (e.g. MEMBER) does not imply write access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPermission {
    Admin,
    Write,
    Read,
    None,
}

impl RepoPermission {
    /// Map GitHub's coarse `permission` field (admin/write/read/none; maintain and
    /// triage collapse into write/read).
    fn from_api(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            "write" | "maintain" => Self::Write,
            "read" | "triage" => Self::Read,
            _ => Self::None,
        }
    }

    /// Whether this level may drive the queue (add/remove PRs).
    pub fn can_write(self) -> bool {
        matches!(self, Self::Admin | Self::Write)
    }
}

#[async_trait]
pub trait RepoClient: Send + Sync {
    /// Open PRs that are candidates for the queue.
    async fn list_open_pulls(&self, repo: &RepoId) -> Result<Vec<PullSummary>, GitHubError>;

    /// Current tip sha of `base` (e.g. `main`). Recorded at stage time to detect races.
    async fn base_sha(&self, repo: &RepoId, base: &str) -> Result<String, GitHubError>;

    /// A single PR's live summary. Resolved at stage time (so a PR updated after it
    /// was queued stages at its latest head, not a stale sha) and at enqueue time
    /// (to validate the PR's base matches the queue before it's accepted).
    async fn pull(&self, repo: &RepoId, pr: u64) -> Result<PullSummary, GitHubError>;

    /// Merge `head_sha` into `branch` (the merges API), advancing the staging
    /// branch by one PR. Returns `Conflicted` if GitHub refuses the merge (409).
    async fn merge_onto(
        &self,
        repo: &RepoId,
        branch: &str,
        head_sha: &str,
        message: &str,
    ) -> Result<MergeOutcome, GitHubError>;

    /// Force `branch` (e.g. `mq/staging/main`) to point at `sha`.
    async fn force_ref(&self, repo: &RepoId, branch: &str, sha: &str) -> Result<(), GitHubError>;

    /// Delete a ref (clean up a staging branch).
    async fn delete_ref(&self, repo: &RepoId, branch: &str) -> Result<(), GitHubError>;

    /// Aggregate state of `required` checks at `sha` (combined statuses + check-runs).
    async fn check_state(
        &self,
        repo: &RepoId,
        sha: &str,
        required: &[String],
    ) -> Result<CheckState, GitHubError>;

    /// The status/check-run contexts that have reported on `sha` (in any state).
    /// Used to learn which required checks GitHub actually evaluates for a PR —
    /// path-filtered checks that never run won't appear, so the queue won't wait on
    /// them forever.
    async fn reported_contexts(&self, repo: &RepoId, sha: &str)
    -> Result<Vec<String>, GitHubError>;

    /// Advance `base` to `to_sha` (fast-forward) — lands a green batch.
    async fn fast_forward(
        &self,
        repo: &RepoId,
        base: &str,
        to_sha: &str,
    ) -> Result<(), GitHubError>;

    /// Post a comment on a PR (eject reason, status updates).
    async fn comment(&self, repo: &RepoId, pr: u64, body: &str) -> Result<(), GitHubError>;

    /// Ensure a managed label exists in the repo (creating it if missing) so it can
    /// be applied to a PR. An already-present label is a no-op.
    async fn ensure_label(
        &self,
        repo: &RepoId,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubError>;

    /// Add labels to a PR (PRs share the issues labels API).
    async fn add_labels(
        &self,
        repo: &RepoId,
        pr: u64,
        labels: &[String],
    ) -> Result<(), GitHubError>;

    /// Remove a label from a PR. A label that isn't present is not an error.
    async fn remove_label(&self, repo: &RepoId, pr: u64, name: &str) -> Result<(), GitHubError>;

    /// Set the PR's merge-train label for `queue` to `target`, clearing this queue's
    /// other managed labels; `None` removes all of them. The target is added before
    /// its siblings are removed so the PR never flickers label-less. Clearing is
    /// scoped to `queue`'s own label variants, so a PR's labels on other queues are
    /// untouched.
    async fn set_train_label(
        &self,
        repo: &RepoId,
        pr: u64,
        target: Option<TrainLabel>,
        queue: &str,
    ) -> Result<(), GitHubError> {
        if let Some(label) = target {
            self.ensure_label(repo, &label.name(queue), label.color(), label.description())
                .await?;
            self.add_labels(repo, pr, &[label.name(queue)]).await?;
        }
        for other in TrainLabel::ALL {
            if Some(other) != target {
                self.remove_label(repo, pr, &other.name(queue)).await?;
            }
        }
        Ok(())
    }

    /// A user's permission level on the repo, to authorize PR-comment commands.
    async fn user_permission(
        &self,
        repo: &RepoId,
        username: &str,
    ) -> Result<RepoPermission, GitHubError>;

    /// The required status-check contexts on `branch`'s protection (empty when the
    /// branch is unprotected or has none). The queue gates on exactly these.
    async fn required_checks(
        &self,
        repo: &RepoId,
        branch: &str,
    ) -> Result<Vec<String>, GitHubError>;
}

/// octocrab-backed implementation. Each call resolves an installation client.
pub struct GitHubRepoClient {
    app: super::AppClient,
}

impl GitHubRepoClient {
    pub fn new(app: super::AppClient) -> Self {
        Self { app }
    }

    fn client(&self, repo: &RepoId) -> Result<Octocrab, GitHubError> {
        self.app.installation(repo.installation_id)
    }
}

#[async_trait]
impl RepoClient for GitHubRepoClient {
    async fn list_open_pulls(&self, repo: &RepoId) -> Result<Vec<PullSummary>, GitHubError> {
        let gh = self.client(repo)?;
        let page = gh
            .pulls(&repo.owner, &repo.name)
            .list()
            .state(octocrab::params::State::Open)
            .per_page(100)
            .send()
            .await?;
        Ok(page
            .items
            .into_iter()
            .map(|p| PullSummary {
                number: p.number,
                title: p.title.unwrap_or_default(),
                head_sha: p.head.sha,
                head_ref: p.head.ref_field,
                base_ref: p.base.ref_field,
                mergeable: p.mergeable,
                // TODO: derive from reviews; GitHub doesn't put approval on the PR object.
                approved: false,
            })
            .collect())
    }

    async fn base_sha(&self, repo: &RepoId, base: &str) -> Result<String, GitHubError> {
        let gh = self.client(repo)?;
        let reference = gh
            .repos(&repo.owner, &repo.name)
            .get_ref(&Reference::Branch(base.to_string()))
            .await?;
        match reference.object {
            Object::Commit { sha, .. } => Ok(sha),
            Object::Tag { sha, .. } => Ok(sha),
            _ => Err(GitHubError::Other(format!("ref {base} is not a commit"))),
        }
    }

    async fn pull(&self, repo: &RepoId, pr: u64) -> Result<PullSummary, GitHubError> {
        let gh = self.client(repo)?;
        let pull = gh.pulls(&repo.owner, &repo.name).get(pr).await?;
        Ok(PullSummary {
            number: pull.number,
            title: pull.title.unwrap_or_default(),
            head_sha: pull.head.sha,
            head_ref: pull.head.ref_field,
            base_ref: pull.base.ref_field,
            mergeable: pull.mergeable,
            approved: false,
        })
    }

    async fn merge_onto(
        &self,
        repo: &RepoId,
        branch: &str,
        head_sha: &str,
        message: &str,
    ) -> Result<MergeOutcome, GitHubError> {
        let gh = self.client(repo)?;
        match gh
            .repos(&repo.owner, &repo.name)
            .merge(head_sha, branch)
            .commit_message(message)
            .send()
            .await
        {
            // 201 (merged) and 204 (head already contained) both return Ok — the
            // 204 as Ok(None) — and both mean staging now contains the head. The
            // already-contained case makes re-merging on a resume idempotent.
            Ok(_) => Ok(MergeOutcome::Merged),
            Err(octocrab::Error::GitHub { source, .. }) if source.status_code.as_u16() == 409 => {
                Ok(MergeOutcome::Conflicted)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn force_ref(&self, repo: &RepoId, branch: &str, sha: &str) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        let route = format!(
            "/repos/{}/{}/git/refs/heads/{branch}",
            repo.owner, repo.name
        );
        let body = serde_json::json!({ "sha": sha, "force": true });
        // PATCH updates an existing ref (force); create it if it doesn't exist yet.
        if gh
            .patch::<serde_json::Value, _, _>(&route, Some(&body))
            .await
            .is_err()
        {
            gh.repos(&repo.owner, &repo.name)
                .create_ref(&Reference::Branch(branch.to_string()), sha)
                .await?;
        }
        Ok(())
    }

    async fn delete_ref(&self, repo: &RepoId, branch: &str) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        gh.repos(&repo.owner, &repo.name)
            .delete_ref(&Reference::Branch(branch.to_string()))
            .await?;
        Ok(())
    }

    async fn check_state(
        &self,
        repo: &RepoId,
        sha: &str,
        required: &[String],
    ) -> Result<CheckState, GitHubError> {
        if required.is_empty() {
            return Ok(CheckState::Pending);
        }
        let gh = self.client(repo)?;
        let mut outcomes: HashMap<String, Outcome> = HashMap::new();

        let status_route = format!("/repos/{}/{}/commits/{sha}/status", repo.owner, repo.name);
        let combined: CombinedStatus = gh.get(&status_route, None::<&()>).await?;
        for s in combined.statuses {
            if let Some(ctx) = s.context {
                outcomes.insert(ctx, Outcome::from_status(s.state));
            }
        }

        let runs_route = format!(
            "/repos/{}/{}/commits/{sha}/check-runs",
            repo.owner, repo.name
        );
        let runs: ListCheckRuns = gh.get(&runs_route, None::<&()>).await?;
        for run in runs.check_runs {
            outcomes.insert(
                run.name,
                Outcome::from_conclusion(run.conclusion.as_deref()),
            );
        }

        let mut pending = false;
        for req in required {
            match outcomes.get(req) {
                Some(Outcome::Failure) => return Ok(CheckState::Failure),
                Some(Outcome::Success) => {}
                Some(Outcome::Pending) | None => pending = true,
            }
        }
        Ok(if pending {
            CheckState::Pending
        } else {
            CheckState::Success
        })
    }

    async fn reported_contexts(
        &self,
        repo: &RepoId,
        sha: &str,
    ) -> Result<Vec<String>, GitHubError> {
        let gh = self.client(repo)?;
        let mut ctxs = Vec::new();
        let status_route = format!("/repos/{}/{}/commits/{sha}/status", repo.owner, repo.name);
        let combined: CombinedStatus = gh.get(&status_route, None::<&()>).await?;
        for s in combined.statuses {
            if let Some(ctx) = s.context {
                ctxs.push(ctx);
            }
        }
        let runs_route = format!(
            "/repos/{}/{}/commits/{sha}/check-runs",
            repo.owner, repo.name
        );
        let runs: ListCheckRuns = gh.get(&runs_route, None::<&()>).await?;
        for run in runs.check_runs {
            ctxs.push(run.name);
        }
        Ok(ctxs)
    }

    async fn fast_forward(
        &self,
        repo: &RepoId,
        base: &str,
        to_sha: &str,
    ) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        let route = format!("/repos/{}/{}/git/refs/heads/{base}", repo.owner, repo.name);
        // force:false — GitHub rejects a non-fast-forward update, so if base moved
        // we never clobber commits that landed independently; the batch supersedes.
        let body = serde_json::json!({ "sha": to_sha, "force": false });
        gh.patch::<serde_json::Value, _, _>(&route, Some(&body))
            .await?;
        Ok(())
    }

    async fn comment(&self, repo: &RepoId, pr: u64, body: &str) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        gh.issues(&repo.owner, &repo.name)
            .create_comment(pr, body)
            .await?;
        Ok(())
    }

    async fn ensure_label(
        &self,
        repo: &RepoId,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        match gh
            .issues(&repo.owner, &repo.name)
            .create_label(name, color, description)
            .await
        {
            Ok(_) => Ok(()),
            // 422 = the label already exists; that's the desired end state.
            Err(octocrab::Error::GitHub { source, .. }) if source.status_code.as_u16() == 422 => {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn add_labels(
        &self,
        repo: &RepoId,
        pr: u64,
        labels: &[String],
    ) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        gh.issues(&repo.owner, &repo.name)
            .add_labels(pr, labels)
            .await?;
        Ok(())
    }

    async fn remove_label(&self, repo: &RepoId, pr: u64, name: &str) -> Result<(), GitHubError> {
        let gh = self.client(repo)?;
        match gh
            .issues(&repo.owner, &repo.name)
            .remove_label(pr, name)
            .await
        {
            Ok(_) => Ok(()),
            // 404 = the PR didn't have the label; nothing to remove.
            Err(octocrab::Error::GitHub { source, .. }) if source.status_code.as_u16() == 404 => {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn user_permission(
        &self,
        repo: &RepoId,
        username: &str,
    ) -> Result<RepoPermission, GitHubError> {
        let gh = self.client(repo)?;
        let route = format!(
            "/repos/{}/{}/collaborators/{username}/permission",
            repo.owner, repo.name
        );
        let resp: CollaboratorPermission = gh.get(&route, None::<&()>).await?;
        Ok(RepoPermission::from_api(&resp.permission))
    }

    async fn required_checks(
        &self,
        repo: &RepoId,
        branch: &str,
    ) -> Result<Vec<String>, GitHubError> {
        let gh = self.client(repo)?;
        let enc = encode_branch(branch);
        let mut ctxs: BTreeSet<String> = BTreeSet::new();

        // Classic branch protection.
        let route = format!("/repos/{}/{}/branches/{}", repo.owner, repo.name, enc);
        let info: BranchInfo = gh.get(&route, None::<&()>).await?;
        if let Some(list) = info
            .protection
            .and_then(|p| p.required_status_checks)
            .and_then(|r| r.contexts)
        {
            ctxs.extend(list);
        }

        // Rulesets — the modern equivalent; a repo may gate via either or both.
        let rules_route = format!("/repos/{}/{}/rules/branches/{}", repo.owner, repo.name, enc);
        let rules: Vec<BranchRule> = gh.get(&rules_route, None::<&()>).await?;
        for rule in rules {
            if let Some(params) = rule.parameters {
                for check in params.required_status_checks.unwrap_or_default() {
                    ctxs.insert(check.context);
                }
            }
        }

        Ok(ctxs.into_iter().collect())
    }
}

/// Percent-encode a branch name as a single path segment, so a slashed branch
/// (e.g. `release/2026`) doesn't split the `/branches/{branch}` route. (Git-ref
/// routes like `git/refs/heads/...` keep the slashes — there they're part of the
/// ref path — so only this branch-resource lookup needs encoding.)
fn encode_branch(branch: &str) -> String {
    branch
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The coarse `permission` field of the collaborator-permission API response.
#[derive(Deserialize)]
struct CollaboratorPermission {
    permission: String,
}

/// Subset of `GET /repos/{owner}/{repo}/branches/{branch}` — the branch protection's
/// required status-check contexts. Every level is optional (an unprotected branch
/// omits `protection`), which `Option` handles without a serde default.
#[derive(Deserialize)]
struct BranchInfo {
    protection: Option<BranchProtection>,
}

#[derive(Deserialize)]
struct BranchProtection {
    required_status_checks: Option<RequiredStatusChecks>,
}

#[derive(Deserialize)]
struct RequiredStatusChecks {
    contexts: Option<Vec<String>>,
}

/// One rule from `GET /repos/{o}/{r}/rules/branches/{branch}` (ruleset rules). Only
/// `required_status_checks` rules carry contexts; other rule types deserialize with
/// `required_status_checks: None` and contribute nothing.
#[derive(Deserialize)]
struct BranchRule {
    parameters: Option<RuleParams>,
}

#[derive(Deserialize)]
struct RuleParams {
    required_status_checks: Option<Vec<RuleCheck>>,
}

#[derive(Deserialize)]
struct RuleCheck {
    context: String,
}

/// One required check's resolved verdict, folded from a commit status or check-run.
#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Failure,
    Pending,
}

impl Outcome {
    fn from_status(state: StatusState) -> Self {
        match state {
            StatusState::Success => Self::Success,
            StatusState::Failure | StatusState::Error => Self::Failure,
            StatusState::Pending => Self::Pending,
            _ => Self::Pending,
        }
    }

    /// A check-run's `conclusion` (None until it completes). Skipped/neutral pass.
    fn from_conclusion(conclusion: Option<&str>) -> Self {
        match conclusion {
            None => Self::Pending,
            Some("success") | Some("skipped") | Some("neutral") => Self::Success,
            Some(_) => Self::Failure,
        }
    }
}
