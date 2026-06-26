//! mergequeue backend: REST API (for the UI) + webhook intake + the queue
//! engine driven by a background worker. See CLAUDE.md for the architecture.

// Scaffold stage: some types/methods aren't wired to a caller yet. Remove once
// the MVP loop is connected end-to-end.
#![allow(dead_code, unused_imports)]

mod api;
mod auth;
mod config;
mod error;
mod github;
mod queue;
mod runtime;
mod setup;
mod store;
mod webhook;

use std::sync::Arc;
use std::time::Duration;

use poem::listener::TcpListener;
use poem::{EndpointExt, Route, Server, get, post};
use poem_openapi::OpenApiService;
use sea_orm::Database;
use secrecy::ExposeSecret;

use migration::{Migrator, MigratorTrait};

use crate::runtime::Runtime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,mergequeue=debug".into()),
        )
        .init();

    let cfg = config::Config::load()?;
    tracing::info!(?cfg, "loaded config");

    let db = Database::connect(cfg.database.url.expose_secret()).await?;
    Migrator::up(&db, None).await?;

    // Hot-swappable engine + webhook secret. Built now if the App is already
    // registered; the /setup callback rebuilds it post-startup without a restart.
    let rt = Arc::new(Runtime::new(cfg.clone(), db.clone()));
    match rt.reinit().await {
        Ok(true) => tracing::info!("github app ready"),
        Ok(false) => tracing::warn!(
            "github app not set up — open {}/setup to create it",
            cfg.server.base_url
        ),
        Err(e) => tracing::error!(error = %e, "github app init failed"),
    }

    // Worker: advance each repo's queue on an interval once the engine exists.
    tokio::spawn({
        let rt = rt.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            let mut n: u32 = 0;
            loop {
                tick.tick().await;
                let Some(engine) = rt.engine().await else {
                    continue;
                };
                // Reconcile installations/repos from the App API every ~minute (and on
                // the first tick) — a backfill for any missed installation webhook.
                if n.is_multiple_of(6)
                    && let Err(e) = rt.sync_installations().await
                {
                    tracing::warn!(error = %e, "worker: installation sync failed");
                }
                n = n.wrapping_add(1);
                let repos = match store::Store::active_repo_ids(&rt.db()).await {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::warn!(error = %e, "worker: listing repos failed");
                        continue;
                    }
                };
                // Sequential per-repo ticks — one FSM step each, serialized.
                for id in repos {
                    if let Err(e) = engine.tick(id).await {
                        tracing::warn!(error = %e, %id, "worker: repo tick failed");
                    }
                }
            }
        }
    });

    let webhook = webhook::Webhook::new(rt.clone());

    let service = OpenApiService::new(
        api::Api {
            cfg: cfg.clone(),
            db: db.clone(),
            rt: rt.clone(),
        },
        "mergequeue",
        "0.1.0",
    )
    .server(format!("{}/api", cfg.server.base_url));
    let swagger = service.swagger_ui();

    let app = Route::new()
        .nest("/api", service)
        .nest("/docs", swagger)
        .at("/setup", get(setup::start))
        .at("/setup/callback", get(setup::callback))
        .at("/webhooks/github", post(webhook::handle))
        .at("/auth/github/login", get(auth::login))
        .at("/auth/github/callback", get(auth::callback))
        .at("/auth/logout", get(auth::logout))
        .data(cfg.clone())
        .data(db.clone())
        .data(rt.clone())
        .data(webhook);

    tracing::info!(port = cfg.server.port, "listening");
    Server::new(TcpListener::bind(("0.0.0.0", cfg.server.port)))
        .run(app)
        .await?;
    Ok(())
}
