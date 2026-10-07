//! Lightweight health endpoint for daemon mode.
//!
//! Binds `0.0.0.0:<port>` and responds to `GET /health` with 200 OK + JSON status.
//! Configure via `[daemon] health_port = 8080` in config.toml.
//!
//! The `cron` block reports scheduler liveness (#1925). The #1893 escalation
//! lives inside the tick loop only, so a loop that never ran (lock denied,
//! panic before the loop, spawn never reached) was silent everywhere else.
//! The loop stamps process-wide instants; this endpoint already runs in the
//! same process, so whatever already polls `/health` (systemd watchdog,
//! uptime monitor) sees the absence without OpenCrabs owning a new timer.

use crate::cron::scheduler::{
    CRON_LAST_TICK_AT, CRON_SPAWNED_AT, CronLiveness, cron_liveness, epoch_secs,
};
use crate::db::{CronJobRepository, Pool};
use axum::extract::State;
use axum::{Json, Router, routing::get};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;

/// Everything the handler reads (#1925). The pool answers "do enabled jobs
/// exist"; the liveness stamps are process-wide statics.
#[derive(Clone)]
struct HealthState {
    pool: Pool,
}

/// The router behind [`serve`], factored out so tests hit the real handler
/// instead of a rebuilt copy of it.
pub(crate) fn router(pool: Pool) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(HealthState { pool })
}

pub async fn serve(port: u16, pool: Pool) -> anyhow::Result<()> {
    let app = router(pool);

    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    tracing::info!("Daemon health endpoint listening on http://{}/health", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<HealthState>) -> Json<serde_json::Value> {
    let repo = CronJobRepository::new(state.pool.clone());
    let enabled = repo.list_enabled().await.map(|jobs| jobs.len() as u64);
    let (cron_state, enabled_jobs, tick_age_secs) = match enabled {
        // A table this reader cannot query is precisely the broken-schedule
        // case; report it instead of defaulting to "no jobs, all calm".
        Err(_) => (
            "jobs-unreadable",
            serde_json::Value::Null,
            serde_json::Value::Null,
        ),
        Ok(n) => {
            let liveness = cron_liveness(
                epoch_secs(),
                CRON_SPAWNED_AT.load(Ordering::Relaxed),
                CRON_LAST_TICK_AT.load(Ordering::Relaxed),
                n,
            );
            match liveness {
                CronLiveness::Idle => ("idle", serde_json::json!(0), serde_json::Value::Null),
                CronLiveness::NotRunningWithJobs { enabled } => (
                    "not-running-with-jobs",
                    serde_json::json!(enabled),
                    serde_json::Value::Null,
                ),
                CronLiveness::Stalled { age_secs, enabled } => (
                    "stalled",
                    serde_json::json!(enabled),
                    serde_json::json!(age_secs),
                ),
                CronLiveness::Healthy { age_secs } => {
                    ("healthy", serde_json::json!(n), serde_json::json!(age_secs))
                }
            }
        }
    };

    Json(serde_json::json!({
        "status": "ok",
        "version": crate::VERSION,
        "mode": "daemon",
        // #1925: the top-level `status` deliberately stays "ok" - monitors use
        // it as process liveness, and flipping it on a cron fault would have
        // systemd restart a live daemon over a dead scheduler. `cron.state` is
        // the scheduler signal; acting on it is the operator's watchdog policy.
        "cron": {
            "state": cron_state,
            "enabled_jobs": enabled_jobs,
            "tick_age_secs": tick_age_secs,
        },
    }))
}
