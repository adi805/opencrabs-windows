//! Loopback HTTP/JSON surface for the Android client (PRD Feature 4, FR-006).
//!
//! Routes:
//! - `GET  /surface/health`                     — liveness
//! - `GET  /surface/sessions`                   — session list
//! - `GET  /surface/sessions/{id}/transcript`   — full transcript for one session
//! - `POST /surface/sessions/{id}/submit`       — idempotent input submit (FR-004)
//! - `GET  /surface/sessions/{id}/events`       — SSE stream of submission/message events
//!
//! Every route except `health` sits behind a bearer token. The listener
//! refuses to start on a non-loopback bind with no token configured, the same
//! gate the A2A gateway uses (#1473): the token is the only authorization
//! boundary in front of a surface that can run the agent with tools.

use crate::brain::agent::service::AgentService;
use crate::config::SessionSurfaceConfig;
use crate::db::Pool;
use crate::db::repository::message::MessageRepository;
use crate::db::repository::session::{SessionListOptions, SessionRepository};
use crate::db::repository::submission::{
    SUBMISSION_DONE, SUBMISSION_FAILED, SUBMISSION_RUNNING, SubmissionRepository,
};
use crate::services::ServiceContext;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, sse},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

/// Shared state for the session surface.
#[derive(Clone)]
pub struct SurfaceState {
    pub pool: Pool,
    pub agent_service: Arc<AgentService>,
    pub api_key: Option<String>,
}

/// Refuse a bind that would expose the agent's tool surface unauthenticated.
///
/// Mirrors `crate::a2a::server::check_gate_authenticated` but names the
/// `[session_surface]` config section in its message. A bind string that is not
/// a bare IP never parses into the `SocketAddr` the server needs either, so
/// treating a parse miss as "not loopback" only ever fails safe.
pub(crate) fn check_surface_gate(bind: &str, api_key: Option<&str>) -> Result<(), String> {
    if api_key.is_some() {
        return Ok(());
    }
    let loopback = bind
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false);
    if loopback {
        return Ok(());
    }
    Err(format!(
        "bind is '{bind}' (not loopback) with no api_key set, which would expose the agent's \
         full tool surface unauthenticated. Set [session_surface].api_key, or bind to 127.0.0.1."
    ))
}

/// Bearer token auth middleware. Skipped when no api_key is configured, which
/// the gate above only ever permits on loopback.
async fn require_bearer(
    State(state): State<SurfaceState>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> axum::response::Response {
    let Some(ref expected) = state.api_key else {
        return next.run(req).await;
    };
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(token) if crate::a2a::server::tokens_match(token, expected) => next.run(req).await,
        // One message for both "no token" and "wrong token": the body must not
        // tell a caller whether a token exists at all (AC-011).
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

/// One error shape for every handler, so the client has a single thing to parse.
pub(crate) struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let ApiError(status, message) = self;
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}

fn internal(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}

fn parse_session_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| bad_request("session id must be a UUID"))
}

pub(crate) fn build_router(state: SurfaceState, allowed_origins: &[String]) -> Router {
    let cors = if allowed_origins.is_empty() {
        CorsLayer::new()
    } else {
        let origins: Vec<_> = allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new().allow_origin(AllowOrigin::list(origins))
    };

    let protected = Router::new()
        .route("/surface/sessions", get(list_sessions))
        .route("/surface/sessions/{id}/transcript", get(get_transcript))
        .route("/surface/sessions/{id}/submit", post(submit))
        .route("/surface/sessions/{id}/events", get(events))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    Router::new()
        .route("/surface/health", get(health))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Start the session surface.
///
/// Runs as a background task; call from `tokio::spawn`. If `ready` is
/// provided it reports the address the listener actually bound (port 0 when
/// the surface is disabled) once the socket is listening, or `Err` if gate
/// validation, address parsing, or socket binding fails.
pub async fn start_server(
    config: &SessionSurfaceConfig,
    agent_service: Arc<AgentService>,
    service_context: ServiceContext,
    ready: Option<tokio::sync::oneshot::Sender<anyhow::Result<SocketAddr>>>,
) -> anyhow::Result<()> {
    if !config.enabled {
        tracing::info!("Session surface disabled in config");
        if let Some(tx) = ready {
            // Nothing is listening when the surface is disabled; a zero port
            // says so without inventing an address.
            let _ = tx.send(Ok(SocketAddr::from(([127, 0, 0, 1], 0))));
        }
        return Ok(());
    }

    if let Err(reason) = check_surface_gate(&config.bind, config.api_key.as_deref()) {
        let msg = anyhow::anyhow!("Session surface refuses to start: {reason}");
        if let Some(tx) = ready {
            let _ = tx.send(Err(anyhow::anyhow!("{msg}")));
        }
        anyhow::bail!(msg);
    }

    let state = SurfaceState {
        pool: service_context.pool(),
        agent_service,
        api_key: config.api_key.clone(),
    };

    let app = build_router(state, &config.allowed_origins);
    let addr: SocketAddr = match format!("{}:{}", config.bind, config.port).parse() {
        Ok(addr) => addr,
        Err(e) => {
            let err = anyhow::anyhow!("Invalid session surface address: {e}");
            if let Some(tx) = ready {
                let _ = tx.send(Err(anyhow::anyhow!("{err}")));
            }
            return Err(err);
        }
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let err = anyhow::anyhow!("Session surface bind failed: {e}");
            if let Some(tx) = ready {
                let _ = tx.send(Err(anyhow::anyhow!("{err}")));
            }
            return Err(err);
        }
    };

    // Report the address the socket actually bound, not the one we asked for:
    // with `port = 0` the OS picks the port, and a caller that asserts the
    // listener is loopback needs the real address.
    let bound = listener.local_addr()?;
    tracing::info!("Session surface listening on http://{}", bound);
    if let Some(tx) = ready {
        let _ = tx.send(Ok(bound));
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// GET /surface/health
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// GET /surface/sessions — the session list the app shows on launch.
async fn list_sessions(
    State(state): State<SurfaceState>,
) -> Result<Json<Vec<crate::db::models::Session>>, ApiError> {
    let repo = SessionRepository::new(state.pool.clone());
    let options = SessionListOptions {
        limit: Some(100),
        ..Default::default()
    };
    let sessions = repo
        .list(options)
        .await
        .map_err(|e| internal(format!("failed to list sessions: {e}")))?;
    Ok(Json(sessions))
}

/// GET /surface/sessions/{id}/transcript — the committed transcript.
///
/// Reads `messages`, which is what the PRD calls "entries" (there is no
/// `entries` table; see session-surface-recon.md).
async fn get_transcript(
    State(state): State<SurfaceState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::db::models::Message>>, ApiError> {
    let session_id = parse_session_id(&id)?;
    let repo = MessageRepository::new(state.pool.clone());
    let messages = repo
        .list_by_session(session_id)
        .await
        .map_err(|e| internal(format!("failed to read transcript: {e}")))?;
    Ok(Json(messages))
}

/// Response body for a submit.
#[derive(Debug, Serialize)]
struct SubmitResponse {
    request_id: String,
    state: String,
    /// `false` when this `requestId` was already claimed, i.e. the caller
    /// retried and is reading back the original submission (FR-004).
    created: bool,
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubmitRequest {
    /// Caller-supplied idempotency key. A retry with the same id must not
    /// start a second run.
    request_id: String,
    input: String,
    model: Option<String>,
}

/// POST /surface/sessions/{id}/submit
///
/// Claims the `request_id` first (FR-004). Only the claim that created the row
/// spawns a run; a repeat returns the existing submission untouched. This is
/// the first production caller of the submission repository, which until now
/// had its contract and tests but no consumer.
async fn submit(
    State(state): State<SurfaceState>,
    Path(id): Path<String>,
    Json(req): Json<SubmitRequest>,
) -> Result<(StatusCode, Json<SubmitResponse>), ApiError> {
    if req.request_id.trim().is_empty() {
        return Err(bad_request("request_id must not be empty"));
    }
    let session_id = parse_session_id(&id)?;
    let repo = SubmissionRepository::new(state.pool.clone());

    let (submission, created) = repo
        .claim(&req.request_id, &id)
        .await
        .map_err(|e| internal(format!("failed to claim submission: {e}")))?;

    if created {
        let agent = state.agent_service.clone();
        let repo = repo.clone();
        let request_id = req.request_id.clone();
        let input = req.input.clone();
        let model = req.model.clone();
        tokio::spawn(async move {
            run_submission(agent, repo, session_id, request_id, input, model).await;
        });
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitResponse {
            request_id: submission.request_id,
            state: submission.state,
            created,
            message_id: submission.message_id,
        }),
    ))
}

/// Drive one claimed submission to a terminal state.
///
/// State moves are best-effort writes on a run that has already been claimed:
/// a failure here must not lose the run, so only the run's own result decides
/// `done` versus `failed`.
async fn run_submission(
    agent: Arc<AgentService>,
    repo: SubmissionRepository,
    session_id: Uuid,
    request_id: String,
    input: String,
    model: Option<String>,
) {
    if let Err(e) = repo.set_state(&request_id, SUBMISSION_RUNNING, None).await {
        tracing::warn!("submission {request_id}: failed to mark running: {e}");
    }

    match agent.send_message(session_id, input, model).await {
        Ok(response) => {
            let message_id = response.message_id.to_string();
            if let Err(e) = repo
                .set_state(&request_id, SUBMISSION_DONE, Some(&message_id))
                .await
            {
                tracing::warn!("submission {request_id}: failed to mark done: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("submission {request_id} failed: {e}");
            if let Err(e) = repo.set_state(&request_id, SUBMISSION_FAILED, None).await {
                tracing::warn!("submission {request_id}: failed to mark failed: {e}");
            }
        }
    }
}

/// GET /surface/sessions/{id}/events — SSE stream.
///
/// Emits `submission` events on state change and `message` events for new
/// transcript rows, then `done` once at least one submission has been seen and
/// none of them is still queued or running. Polling rather than an in-process
/// broadcast channel keeps the surface correct across a process restart: the
/// state it reads is the state the DB holds, so a client that reconnects after
/// the core was killed re-attaches to the same truth.
async fn events(
    State(state): State<SurfaceState>,
    Path(id): Path<String>,
) -> Result<
    sse::Sse<impl futures::Stream<Item = Result<sse::Event, std::convert::Infallible>>>,
    ApiError,
> {
    let session_id = parse_session_id(&id)?;
    let pool = state.pool.clone();

    let stream = futures::stream::unfold(
        EventCursor::new(pool, session_id, id),
        |mut cursor| async move {
            loop {
                match cursor.step().await {
                    Ok(Some(event)) => return Some((Ok(event), cursor)),
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                    Err(e) => {
                        tracing::warn!("session surface event stream error: {e}");
                        return None;
                    }
                }
            }
        },
    );

    Ok(sse::Sse::new(stream))
}

/// Tracks what the event stream has already emitted, so each poll only sends
/// the delta.
struct EventCursor {
    pool: Pool,
    session_id: Uuid,
    session_id_raw: String,
    seen_states: std::collections::HashMap<String, String>,
    last_sequence: i32,
    saw_any_submission: bool,
}

impl EventCursor {
    fn new(pool: Pool, session_id: Uuid, session_id_raw: String) -> Self {
        Self {
            pool,
            session_id,
            session_id_raw,
            seen_states: std::collections::HashMap::new(),
            last_sequence: i32::MIN,
            saw_any_submission: false,
        }
    }

    /// One poll. `Ok(Some(event))` yields, `Ok(None)` means "nothing new yet".
    async fn step(&mut self) -> anyhow::Result<Option<sse::Event>> {
        let submissions = SubmissionRepository::new(self.pool.clone())
            .list_for_session(&self.session_id_raw)
            .await?;

        let mut all_terminal = !submissions.is_empty();
        for submission in &submissions {
            self.saw_any_submission = true;
            let changed = self
                .seen_states
                .get(&submission.request_id)
                .map(|prev| prev != &submission.state)
                .unwrap_or(true);
            if submission.state != SUBMISSION_DONE && submission.state != SUBMISSION_FAILED {
                all_terminal = false;
            }
            if changed {
                self.seen_states
                    .insert(submission.request_id.clone(), submission.state.clone());
                let payload = serde_json::json!({
                    "request_id": submission.request_id,
                    "state": submission.state,
                    "message_id": submission.message_id,
                });
                return Ok(Some(
                    sse::Event::default()
                        .event("submission")
                        .data(payload.to_string()),
                ));
            }
        }

        let messages = MessageRepository::new(self.pool.clone())
            .list_by_session(self.session_id)
            .await?;
        let fresh: Vec<_> = messages
            .iter()
            .filter(|m| m.sequence > self.last_sequence)
            .collect();
        if let Some(message) = fresh.first() {
            self.last_sequence = message.sequence;
            let payload = serde_json::json!({
                "id": message.id,
                "role": message.role,
                "content": message.content,
                "sequence": message.sequence,
            });
            return Ok(Some(
                sse::Event::default()
                    .event("message")
                    .data(payload.to_string()),
            ));
        }

        if self.saw_any_submission && all_terminal {
            let payload = serde_json::json!({ "session_id": self.session_id_raw });
            return Ok(Some(
                sse::Event::default()
                    .event("done")
                    .data(payload.to_string()),
            ));
        }

        Ok(None)
    }
}
