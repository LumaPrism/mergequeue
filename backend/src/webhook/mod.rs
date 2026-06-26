//! GitHub webhook intake: verify the HMAC signature (constant-time), then route
//! by event type into typed payloads. Installation events (de)provision
//! installations + repos via the `Store`; a PR close drops the PR from the queue;
//! `issue_comment` parses `/mergequeue queue`-style commands from PR comments.
//! check_run/push will nudge the engine once the worker is wired.

use std::sync::Arc;

use hmac::{Hmac, Mac};
use poem::http::StatusCode;
use poem::{Error, FromRequest, Request, RequestBody, handler};
use sea_orm::{DatabaseConnection, DbErr};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::runtime::{Enqueued, Removed, Runtime, SecretCell};
use crate::store::Store;

/// Verify `X-Hub-Signature-256: sha256=<hex>` against the raw body.
pub fn verify_signature(secret: &SecretString, signature: &str, body: &[u8]) -> bool {
    let Some(hex_sig) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.expose_secret().as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = mac.finalize().into_bytes();
    computed.ct_eq(expected.as_slice()).into()
}

/// Webhook events we act on. Everything else is acknowledged and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelevantEvent {
    /// App installed / repos added or removed → (de)provision repos.
    Installation,
    /// PR opened/synchronized/closed → refresh cached PR state.
    PullRequest,
    /// A check-run finished → re-tick the repo whose staging sha it targets.
    CheckRun,
    /// Direct push to a base branch → re-tick (may trigger a re-stage).
    Push,
    /// A PR comment → parse a `/mergequeue` queue command.
    IssueComment,
}

impl RelevantEvent {
    pub fn from_header(event: &str) -> Option<Self> {
        match event {
            "installation" | "installation_repositories" => Some(Self::Installation),
            "pull_request" => Some(Self::PullRequest),
            "check_run" => Some(Self::CheckRun),
            "push" => Some(Self::Push),
            "issue_comment" => Some(Self::IssueComment),
            _ => None,
        }
    }
}

/// A queue command parsed from a PR comment body. Written on its own line as
/// `/mergequeue queue`, `/mq queue`, or the bare `/queue` (likewise `dequeue`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Queue,
    Dequeue,
}

impl Command {
    /// First command found across the comment's lines, or `None`. A command must
    /// lead its line — `/mergequeue <verb>`, `/mq <verb>`, or the bare `/<verb>` —
    /// and lines inside fenced code blocks (``` or ~~~) are ignored.
    fn parse(body: &str) -> Option<Self> {
        let mut in_fence = false;
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            let mut tok = line.split_whitespace();
            let verb = match tok.next() {
                Some("/mergequeue") | Some("/mq") => match tok.next() {
                    Some(v) => v,
                    None => continue,
                },
                Some("/queue") => "queue",
                Some("/dequeue") | Some("/unqueue") => "dequeue",
                _ => continue,
            };
            match verb {
                "queue" => return Some(Self::Queue),
                "dequeue" | "unqueue" => return Some(Self::Dequeue),
                _ => continue,
            }
        }
        None
    }
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
}

impl Repository {
    /// Split `owner/name`; `None` for a malformed full name.
    fn split(&self) -> Option<(&str, &str)> {
        self.full_name.split_once('/')
    }
}

#[derive(Deserialize)]
struct Account {
    login: String,
}

#[derive(Deserialize)]
struct Installation {
    id: i64,
    account: Account,
}

/// `installation` and `installation_repositories` events. The repo lists are
/// present only on their respective subtypes, hence `Option` (not serde default).
#[derive(Deserialize)]
struct InstallationEvent {
    action: String,
    installation: Installation,
    repositories: Option<Vec<Repository>>,
    repositories_added: Option<Vec<Repository>>,
    repositories_removed: Option<Vec<Repository>>,
}

#[derive(Deserialize)]
struct PullRequestEvent {
    action: String,
    number: i64,
    repository: Repository,
}

/// `issue_comment` event. PR comments arrive here too (a PR is an issue); the
/// `issue.pull_request` link is present only when the comment is on a PR.
#[derive(Deserialize)]
struct IssueCommentEvent {
    action: String,
    issue: Issue,
    comment: Comment,
    repository: Repository,
}

#[derive(Deserialize)]
struct Issue {
    number: i64,
    /// "open" or "closed" — a queue command on a closed PR is rejected (GitHub
    /// still delivers `issue_comment` for closed PRs).
    state: String,
    /// Present only when this issue is a pull request.
    pull_request: Option<PullRequestLink>,
}

/// Presence marker for `issue.pull_request` — its fields are unused; whether it
/// exists is what distinguishes a PR comment from a plain-issue comment.
#[derive(Deserialize)]
struct PullRequestLink {}

#[derive(Deserialize)]
struct Comment {
    body: String,
    user: CommentUser,
}

#[derive(Deserialize)]
struct CommentUser {
    login: String,
}

/// Webhook intake state: the DB plus the shared webhook-secret cell. The secret
/// is `None` until the App is registered (then requests get 503); the cell lets a
/// post-startup `/setup` make it live without a restart.
#[derive(Clone)]
pub struct Webhook {
    db: DatabaseConnection,
    secret: SecretCell,
    rt: Arc<Runtime>,
}

impl Webhook {
    pub fn new(rt: Arc<Runtime>) -> Self {
        Self {
            db: rt.db(),
            secret: rt.secret_cell(),
            rt,
        }
    }

    async fn on_installation(&self, ev: InstallationEvent) -> Result<(), DbErr> {
        let installation_id = ev.installation.id;
        if ev.action == "deleted" {
            return Store::deprovision_installation(&self.db, installation_id).await;
        }
        Store::provision_installation(&self.db, installation_id, &ev.installation.account.login)
            .await?;

        let mut added = ev.repositories.unwrap_or_default();
        added.extend(ev.repositories_added.unwrap_or_default());
        for repo in added {
            if let Some((owner, name)) = repo.split() {
                Store::upsert_repo(&self.db, installation_id, owner, name).await?;
            }
        }
        for repo in ev.repositories_removed.unwrap_or_default() {
            if let Some((owner, name)) = repo.split() {
                Store::delete_repo(&self.db, owner, name).await?;
            }
        }
        Ok(())
    }

    async fn on_pull_request(&self, ev: PullRequestEvent) -> Result<(), DbErr> {
        if ev.action != "closed" {
            return Ok(());
        }
        let Some((owner, name)) = ev.repository.split() else {
            return Ok(());
        };
        let removed = Store::dequeue_pr(&self.db, owner, name, ev.number).await?;
        if removed && let Some(repo_id) = Store::repo_id_by_name(&self.db, owner, name).await? {
            let _ = self.rt.set_pr_label(repo_id, ev.number as u64, None).await;
        }
        Ok(())
    }

    /// A PR comment: run a `/mergequeue` queue command if present and the commenter
    /// is authorized. Replies on the PR with the outcome. GitHub-side failures
    /// (e.g. posting the reply) are logged, not surfaced — the ack still succeeds.
    async fn on_issue_comment(&self, ev: IssueCommentEvent) -> Result<(), DbErr> {
        if ev.action != "created" || ev.issue.pull_request.is_none() {
            return Ok(());
        }
        if ev.comment.user.login.ends_with("[bot]") {
            return Ok(());
        }
        let Some(cmd) = Command::parse(&ev.comment.body) else {
            return Ok(());
        };
        let Some((owner, name)) = ev.repository.split() else {
            return Ok(());
        };
        let Some(repo_id) = Store::repo_id_by_name(&self.db, owner, name).await? else {
            return Ok(());
        };
        let pr = ev.issue.number as u64;
        let actor = ev.comment.user.login.as_str();

        // Authorize against real repo permission. author_association (e.g. MEMBER)
        // does not imply write access — a read-only org member must not drive the
        // queue. Fail closed if the permission check itself errors.
        let authorized = match self.rt.can_write(repo_id, actor).await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, actor, "permission check failed");
                false
            }
        };
        if !authorized {
            tracing::info!(actor, pr, "ignoring mergequeue command from a non-writer");
            return Ok(());
        }

        // GitHub delivers issue_comment for closed PRs too — never queue a closed PR
        // (its head must not be merged into base).
        if ev.issue.state != "open" {
            if matches!(cmd, Command::Queue) {
                let _ = self
                    .rt
                    .comment(
                        repo_id,
                        pr,
                        &format!("#{pr} is closed — only open PRs can be queued."),
                    )
                    .await;
            }
            return Ok(());
        }

        match cmd {
            Command::Queue => match self.rt.enqueue_pr(repo_id, pr, actor).await {
                // enqueue_pr already comments on the PR ("added to the train"), so
                // there's nothing more to say here.
                Ok(Enqueued::Ok { .. }) => {}
                Ok(Enqueued::WrongBase {
                    pr_base,
                    queue_base,
                }) => {
                    let msg = format!(
                        "⚠️ #{pr} targets `{pr_base}`, but this queue lands into `{queue_base}`. \
                         Only PRs into `{queue_base}` can be queued."
                    );
                    let _ = self.rt.comment(repo_id, pr, &msg).await;
                }
                Err(e) => tracing::warn!(error = %e, pr, "pr-comment queue failed"),
            },
            Command::Dequeue => {
                let outcome = match Store::open_entry_id(&self.db, repo_id, pr).await? {
                    Some(entry_id) => self.rt.force_dequeue(repo_id, entry_id).await.ok(),
                    None => None,
                };
                let msg = match outcome {
                    Some(Removed::Gone { .. }) => None,
                    Some(Removed::Busy { .. }) => Some(
                        "**mergequeue** · can't remove — the batch is merging; try again shortly"
                            .to_string(),
                    ),
                    _ => Some(format!("#{pr} isn't in the merge train.")),
                };
                if let Some(msg) = msg {
                    let _ = self.rt.comment(repo_id, pr, &msg).await;
                }
            }
        }
        Ok(())
    }
}

/// The raw delivery: the webhook state (pulled from request data) plus the two
/// headers and body bytes we verify/route on. Bundling state into one extractor
/// avoids pairing a `Data` extractor with a body extractor, which the `#[handler]`
/// macro rejects.
struct Delivery {
    webhook: Webhook,
    event: String,
    signature: String,
    body: Vec<u8>,
}

impl<'a> FromRequest<'a> for Delivery {
    async fn from_request(req: &'a Request, body: &mut RequestBody) -> poem::Result<Self> {
        let webhook = req.data::<Webhook>().cloned().ok_or_else(|| {
            Error::from_string("webhook state missing", StatusCode::INTERNAL_SERVER_ERROR)
        })?;
        let event = req
            .headers()
            .get("X-GitHub-Event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let signature = req
            .headers()
            .get("X-Hub-Signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = body
            .take()?
            .into_vec()
            .await
            .map_err(|e| Error::from_string(e.to_string(), StatusCode::BAD_REQUEST))?;
        Ok(Self {
            webhook,
            event,
            signature,
            body,
        })
    }
}

/// `POST /webhooks/github` — verify the signature, process the event, ack fast.
#[handler]
pub async fn handle(delivery: Delivery) -> StatusCode {
    let Delivery {
        webhook,
        event,
        signature,
        body,
    } = delivery;

    {
        let guard = webhook.secret.read().await;
        let Some(secret) = guard.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE;
        };
        if !verify_signature(secret, &signature, &body) {
            return StatusCode::UNAUTHORIZED;
        }
    }

    let Some(kind) = RelevantEvent::from_header(&event) else {
        return StatusCode::ACCEPTED;
    };

    let result = match kind {
        RelevantEvent::Installation => match serde_json::from_slice::<InstallationEvent>(&body) {
            Ok(ev) => webhook.on_installation(ev).await,
            Err(_) => return StatusCode::BAD_REQUEST,
        },
        RelevantEvent::PullRequest => match serde_json::from_slice::<PullRequestEvent>(&body) {
            Ok(ev) => webhook.on_pull_request(ev).await,
            Err(_) => return StatusCode::BAD_REQUEST,
        },
        RelevantEvent::IssueComment => match serde_json::from_slice::<IssueCommentEvent>(&body) {
            Ok(ev) => webhook.on_issue_comment(ev).await,
            Err(_) => return StatusCode::BAD_REQUEST,
        },
        RelevantEvent::CheckRun | RelevantEvent::Push => Ok(()),
    };

    match result {
        Ok(()) => StatusCode::ACCEPTED,
        Err(e) => {
            tracing::error!(error = %e, event, "webhook processing failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn test_webhook_command_parses_each_spelling() {
        assert_eq!(Command::parse("/mergequeue queue"), Some(Command::Queue));
        assert_eq!(Command::parse("/mq queue"), Some(Command::Queue));
        assert_eq!(Command::parse("/queue"), Some(Command::Queue));
        assert_eq!(
            Command::parse("/mergequeue dequeue"),
            Some(Command::Dequeue)
        );
        assert_eq!(Command::parse("/mq dequeue"), Some(Command::Dequeue));
        assert_eq!(Command::parse("/dequeue"), Some(Command::Dequeue));
        assert_eq!(Command::parse("/unqueue"), Some(Command::Dequeue));
    }

    #[test]
    fn test_webhook_command_must_lead_its_line() {
        assert_eq!(Command::parse("please /queue this"), None);
        assert_eq!(
            Command::parse("LGTM!\n\n/mq queue\nthanks"),
            Some(Command::Queue)
        );
        assert_eq!(
            Command::parse("   /mergequeue   queue  "),
            Some(Command::Queue)
        );
    }

    #[test]
    fn test_webhook_command_ignores_non_commands() {
        assert_eq!(Command::parse("looks good to me"), None);
        assert_eq!(Command::parse("/help"), None);
        assert_eq!(Command::parse("/mq"), None);
        assert_eq!(Command::parse("/mqqueue"), None);
        assert_eq!(Command::parse("/mergequeueueue queue"), None);
    }

    #[test]
    fn test_webhook_command_ignores_code_fences() {
        assert_eq!(Command::parse("how to:\n```\n/queue\n```\nthanks"), None);
        assert_eq!(Command::parse("~~~\n/mq queue\n~~~"), None);
        assert_eq!(
            Command::parse("```\n/mq queue\n```\n/queue"),
            Some(Command::Queue)
        );
    }
}
