# The engine

The engine is a finite state machine over a **batch** — a set of queued PRs
tested together against the latest base branch. One `Engine::tick(repo)`
performs exactly one transition for that repo and persists the next state
*before* its GitHub side effect, so a crashed/restarted process resumes by
re-dispatching the current persisted state.

## States

`BatchState` (persisted to `batches.state`):

| State | Kind | Meaning |
|-------|------|---------|
| `Staging` | active | assembling the staging branch (base + each PR applied) |
| `Testing` | active | staging pushed; awaiting required checks |
| `Merging` | active | checks green; fast-forwarding base |
| `Bisecting` | active | checks red; narrowing to the culprit |
| `Merged` | terminal | landed on base |
| `Ejected` | terminal | a culprit was ejected; the remainder re-queued |
| `Superseded` | terminal | base moved / abandoned; entries re-queued |

`EntryState` (per PR): `Queued → Testing → (Merged | Ejected)`.

## Transitions

```text
(no batch) --queued PRs?--> Staging
Staging    --assemble+push--> Testing
Testing    --checks pending--> Testing        (wait)
Testing    --checks green----> Merging
Testing    --checks red------> Bisecting
Testing    --base moved------> Superseded      (terminal; re-queue)
Merging    --fast-forward----> Merged          (terminal)
Bisecting  --1 entry left----> Ejected         (terminal; eject culprit)
Bisecting  --> 1 entry-------> Staging          (smaller batch; re-queue the rest)
```

The race guard (base moved) is checked at the top of every `step` once the batch
has been staged, so any active state can transition to `Superseded`.

## Invariants

- **At most one non-terminal batch per repo** — enforced by the partial unique
  index `batches_one_active` (`state IN (staging,testing,merging,bisecting)`).
- **Persist-before-side-effect** — each transition is a single store write
  followed by the GitHub call; a crash between them re-runs the call idempotently
  on resume (the staging branch is disposable and re-derived from base).
- **Convergence of bisect** — a failing batch of N is split, the first half
  re-tested as a smaller batch and the rest re-queued; halving terminates at a
  single-entry batch, which is ejected. (Optimal bisect — keep a passing prefix,
  re-test only the suffix — is a planned refinement.)

## The CI-agnostic contract

The engine never talks to a CI. It pushes a staging branch `mq/staging/<base>`
and reads the repo's **required GitHub check-runs / commit statuses** on the
staging tip. The only integration requirement is that the repo's CI runs on
`mq/**` branches — identical to how GitHub's native merge queue uses
`gh-readonly-queue/**`. Any CI that reports a check (Woodpecker, GitHub Actions,
Buildkite, …) works unchanged.

## Testability

`Engine` depends on two traits — `RepoClient` (GitHub I/O) and `QueueStore`
(persistence) — never on concrete clients. Unit tests drive the FSM with
in-memory fakes and assert the `TickOutcome` sequence (e.g. `Staged → Waiting →
Merged`, or a red batch producing `Bisecting → Ejected`).
