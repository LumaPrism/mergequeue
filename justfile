# mergequeue — local dev. Tools (node, just, smee-client) come from mise:
#   mise install
# Per-developer secrets (your smee.io channel) live in .env (gitignored).

smee_target := "http://localhost:8080/webhooks/github"

# list recipes
default:
    @just --list

# the whole stack in one terminal — Postgres, backend, web, and (if MQ_SMEE_URL is set) the webhook tunnel. Ctrl-C stops all of them.
dev: db
    #!/usr/bin/env bash
    set -uo pipefail
    trap 'echo; echo "↩ stopping dev stack"; kill 0' EXIT
    echo "▶ backend   http://localhost:8080"
    ( cd backend && cargo run ) &
    echo "▶ web       http://localhost:3001"
    ( cd web && PORT=3001 pnpm dev ) &
    if [ -n "${MQ_SMEE_URL:-}" ]; then
        echo "▶ webhooks  ${MQ_SMEE_URL} → {{smee_target}}"
        smee --url "${MQ_SMEE_URL}" --target "{{smee_target}}" &
    else
        echo "· webhooks  skipped (set MQ_SMEE_URL in .env to forward GitHub events)"
    fi
    wait -n

# Postgres for local dev
db:
    MQ_PG_PORT="${MQ_PG_PORT:-5433}" docker compose up -d --wait

# backend API + queue engine (reads MQ_* from the mise env)
backend:
    cd backend && cargo run

# the dashboard (port 3001 to match MQ_SERVER__APP_URL)
web:
    cd web && PORT=3001 pnpm dev

# forward GitHub webhooks (smee.io) → the local backend
smee:
    smee --url "${MQ_SMEE_URL:?set MQ_SMEE_URL in .env (your smee.io channel)}" --target "{{smee_target}}"

# regenerate the shared TS types from the Rust backend
regen-types:
    cd web && just regen-types
