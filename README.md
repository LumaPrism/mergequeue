# mergequeue

A self-hostable, **CI-agnostic merge-queue GitHub App** with a management UI.

It batches approved PRs, tests the **combined** result against the latest base branch via *your existing CI*, and merges the batch — or bisects to eject the one PR that breaks. No more "green PR breaks main because something else merged ahead of it."

> Works on any GitHub plan, any CI, self-hosted.

## How it works

You install the app on a repo and drive a queue from the dashboard. For each batch the engine:

1. Stages a branch `mq/staging/<base>` = latest base + the batched PRs.
2. Waits for **your** required check-runs to report on it (configure your CI to run on `mq/**` branches — same as GitHub's `gh-readonly-queue/**`).
3. Green → merge the batch and advance. Red → bisect, eject the culprit, re-queue the rest. Base moved → re-stage.

CI-agnostic: it only reads GitHub check-runs. Woodpecker, GitHub Actions, Buildkite — anything that reports a status works.

## Stack

- **Backend**: Rust (Poem + poem-openapi, SeaORM/Postgres, Apalis worker, octocrab for the GitHub App).
- **Frontend**: Next.js + Radix + Tailwind.

## Run the service

```sh
docker compose up -d                 # Postgres
cp .env.example .env                 # the GitHub App is created from the UI (step 1)
cd backend && cargo run              # REST API + engine worker → :8080
cd web && pnpm install && pnpm dev   # dashboard → :3001
```

For webhooks to reach a local instance, tunnel them with [smee.io](https://smee.io) and set `MQ_SMEE_URL` in `.env`. (`just dev` runs all of the above at once.)

## Set up a repo (minimal steps)

1. **Create & install the GitHub App.** Open `https://<your-host>/setup` and confirm the manifest — GitHub creates the App with the right permissions and hands the credentials back automatically (no copying private keys by hand), then offers to install it on your org/repos.
2. **Enable the repo** in the dashboard (`/app`). mergequeue reads its branch protection / rulesets to find the **base branch** and its **required checks**. *A repo with no required checks is held — the queue never lands anything ungated.*
3. **Point your CI at the staging branch.** Your existing CI must run on `mq/staging/**` and report its required checks there — the one integration contract. Woodpecker / GitHub Actions snippets: [docs → CI integration](web/content/docs/ci-integration.mdx). *If your CI is wired for GitHub's native queue (`gh-readonly-queue/**` / the `merge_group` event), add `mq/staging/**` too — those triggers don't fire for mergequeue.*
4. **Let the App land merges.** To land a batch, mergequeue fast-forwards your base branch. If the base is protected by a ruleset that requires a PR, add the **mergequeue App as a bypass actor** (Settings → Rules → Rulesets → Bypass list → Allow). Without it, batches pass CI but can't land — mergequeue says so with a `merge-queue: blocked` comment + label instead of retrying silently.
5. **Queue PRs** from the dashboard, or comment **`/mq queue`** on a PR (**`/mq dequeue`** to remove). mergequeue stages the batch, tests the combined result via your CI, and lands it — or bisects to eject the breaker and re-queues the rest.

To create the App by hand instead of the manifest flow, it needs **Contents: read/write, Pull requests: read/write, Issues: write, Checks: read, Commit statuses: read, Administration: read, Metadata: read** and the `pull_request`, `check_run`, `status`, `push`, `issue_comment`, `installation`, `installation_repositories` events.

## License

MIT (intended OSS).
