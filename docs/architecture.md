# Architecture

mergequeue is a self-hostable GitHub App: a Rust backend (queue engine + REST API
+ webhook intake) and a Next.js dashboard. It is CI-agnostic — it reads GitHub
check-runs and never runs builds itself.

```
GitHub  ──webhooks──▶  backend (Rust)
   ▲                     ├─ github/   App auth (JWT→installation token), RepoClient
   │  REST: refs,        ├─ webhook/  HMAC verify + event routing
   │  check-runs, merge  ├─ queue/    domain model + engine FSM (see engine.md)
   └─────────────────────┤─ store/    Postgres (SeaORM) behind a QueueStore trait
                         └─ api/      REST for the dashboard
                              │
                           Postgres ◀── Apalis worker (drives tick() async)
                              ▲
   Next.js dashboard ─────────┘ REST
```

## Components

- **github/** — `AppClient` authenticates as the App (RS256 JWT) and mints
  per-installation clients (octocrab). `RepoClient` is the trait the engine uses
  (list PRs, base sha, apply PR to staging, force/delete ref, check state,
  fast-forward, comment). One installation token per repo, minted on demand.
- **queue/** — storage-agnostic domain types and the `Engine` FSM. See
  [engine.md](./engine.md).
- **store/** — `QueueStore` trait + `PgStore` (SeaORM). Single persistence seam;
  each FSM transition is one store call.
- **webhook/** — verifies `X-Hub-Signature-256` (constant-time), maps events to
  a `RelevantEvent`, and enqueues a tick for the affected repo (low latency vs.
  the poll interval).
- **api/** — poem-openapi REST (typed spec + Swagger at `/docs`) consumed by the
  dashboard. Handlers delegate to `QueueStore`; the engine runs in the worker.

## Data model

`installations` (one per GitHub App install) → `repos` (per-repo queue config:
base branch, batch size, required checks, merge method, staging prefix) →
`queue_entries` (the queue) and `batches` (FSM state + staging refs), linked by
`batch_entries`.

## Multi-tenancy

One row per installation; we store only the `installation_id` and mint tokens on
demand (never persist tokens). Repos are scoped to their installation.

## Deployment

`docker compose up` runs Postgres, the backend, and the dashboard. The backend
needs a GitHub App (App ID, private key, webhook secret, client id/secret) — see
the README for the App's permissions and event subscriptions.

### Secrets at rest

The DB-stored GitHub App secrets (`github_app.private_key`, `client_secret`,
`webhook_secret`) are encrypted with ChaCha20-Poly1305 using `MQ_SECRET__KEY` — a
32-char key held outside Postgres (env or `MQ_SECRET__KEY_FILE`). Each value is
stored as `enc:<hex(nonce‖ciphertext‖tag)>`, so a leaked DB dump is undecryptable
without the key. The key is required before `/setup` will register an App; a
pre-existing plaintext row is re-encrypted in place on the next `reinit`. There is
no key rotation — losing the key means re-registering the App. The static config
escape hatch (`MQ_GITHUB__*`) stays plaintext on the host filesystem by the
operator's choice (the normal 12-factor posture); it never touches the DB.

## Conventions

Code organisation: module-per-concern, a store pattern for all DB access,
`thiserror` per-module errors, `secrecy` for secrets, and OpenAPI metadata.
