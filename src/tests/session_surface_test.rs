//! The session surface must never come up as an open, unauthenticated gate,
//! and a request without a valid bearer token must be refused without the body
//! revealing whether a token exists at all (AC-011, AC-024).
//!
//! The surface can run the agent with tools, so the token is the only
//! authorization boundary in front of it. Loopback is safe; any other bind with
//! no token is an open gate, and the startup guard must refuse it rather than
//! start silently open.

use crate::session::surface::*;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use tower::ServiceExt;

// ── the startup guard ───────────────────────────────────────────────────────

#[test]
fn loopback_without_a_token_is_allowed() {
    // Same-box callers only; this is the safe default posture.
    assert!(check_surface_gate("127.0.0.1", None).is_ok());
    assert!(check_surface_gate("::1", None).is_ok());
}

#[test]
fn a_token_makes_any_bind_allowed() {
    // A configured token is the authorization boundary, so the bind is free.
    assert!(check_surface_gate("0.0.0.0", Some("secret")).is_ok());
    assert!(check_surface_gate("192.168.1.10", Some("secret")).is_ok());
}

#[test]
fn a_wildcard_bind_without_a_token_is_refused() {
    // 0.0.0.0 is every interface: the exact open-gate case.
    let err = check_surface_gate("0.0.0.0", None).unwrap_err();
    assert!(
        err.contains("not loopback"),
        "message names the cause: {err}"
    );
    assert!(err.contains("api_key"), "message says how to fix it: {err}");
    assert!(
        err.contains("session_surface"),
        "message names the right config section: {err}"
    );
}

#[test]
fn a_public_ip_without_a_token_is_refused() {
    assert!(check_surface_gate("192.168.1.10", None).is_err());
    assert!(check_surface_gate("10.0.0.5", None).is_err());
    assert!(check_surface_gate("0.0.0.0", None).is_err());
}

#[test]
fn a_hostname_bind_fails_safe() {
    // A non-IP bind never parses into the SocketAddr the server needs, so
    // treating a parse miss as "not loopback" only ever fails safe.
    assert!(check_surface_gate("localhost", None).is_err());
    assert!(check_surface_gate("example.com", None).is_err());
}

// ── the bearer middleware ───────────────────────────────────────────────────

async fn test_state_with_token(token: Option<&str>) -> SurfaceState {
    use crate::a2a::test_helpers::helpers;
    SurfaceState {
        pool: helpers::placeholder_service_context().await.pool(),
        agent_service: helpers::placeholder_agent_service().await,
        api_key: token.map(|t| t.to_string()),
    }
}

#[tokio::test]
async fn health_needs_no_token() {
    let app = build_router(test_state_with_token(Some("secret")).await, &[]);
    let req = Request::builder()
        .uri("/surface/health")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_missing_token_is_unauthorized() {
    let app = build_router(test_state_with_token(Some("secret")).await, &[]);
    let req = Request::builder()
        .uri("/surface/sessions")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_wrong_token_is_unauthorized() {
    let app = build_router(test_state_with_token(Some("secret")).await, &[]);
    let req = Request::builder()
        .uri("/surface/sessions")
        .header("authorization", "Bearer not-the-secret")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_unauthorized_body_does_not_reveal_whether_a_token_exists() {
    // A caller must not be able to tell "no token configured" from "wrong
    // token" by reading the response, or the surface leaks its own posture.
    let app = build_router(test_state_with_token(Some("secret")).await, &[]);

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/surface/sessions")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let wrong = app
        .oneshot(
            Request::builder()
                .uri("/surface/sessions")
                .header("authorization", "Bearer not-the-secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(missing.status(), wrong.status());
    let missing_body = axum::body::to_bytes(missing.into_body(), 4096)
        .await
        .expect("body");
    let wrong_body = axum::body::to_bytes(wrong.into_body(), 4096)
        .await
        .expect("body");
    assert_eq!(
        missing_body, wrong_body,
        "the two refusal bodies must be byte-identical"
    );
}

#[tokio::test]
async fn a_correct_token_reaches_the_handler() {
    let app = build_router(test_state_with_token(Some("secret")).await, &[]);
    let req = Request::builder()
        .uri("/surface/sessions")
        .header("authorization", "Bearer secret")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    // Reached the handler and listed sessions (none in a fresh DB).
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── the whole path over a socket (FR-004, FR-006, AC-010) ───────────────────

/// One live server on an ephemeral loopback port, one real session, and a stub
/// provider: submit, stream and transcript all go over HTTP, with no key and no
/// network. `MockProvider` answers without touching a provider, so the run
/// reaches `done` and the transcript grows a real assistant row.
#[tokio::test]
async fn the_surface_serves_submit_stream_and_transcript_over_a_socket() {
    use crate::a2a::test_helpers::helpers;
    use crate::brain::agent::service::AgentService;
    use crate::config::SessionSurfaceConfig;
    use crate::db::models::{Message, Session};
    use crate::db::repository::message::MessageRepository;
    use crate::db::repository::session::SessionRepository;
    use crate::db::repository::submission::SubmissionRepository;
    use crate::tests::agent_service_mocks::MockProvider;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let ctx = helpers::placeholder_service_context().await;
    let pool = ctx.pool();

    let session = Session::new(Some("surface e2e".to_string()), None, None);
    SessionRepository::new(pool.clone())
        .create(&session)
        .await
        .expect("create session");
    let seeded = Message::new(
        session.id,
        "user".to_string(),
        "hello from the app".to_string(),
        1,
    );
    MessageRepository::new(pool.clone())
        .create(&seeded)
        .await
        .expect("seed one transcript row");

    // The agent shares this pool, so the run it drives writes to the DB the
    // surface reads back.
    let agent = Arc::new(AgentService::new_for_test(Arc::new(MockProvider), ctx.clone()).await);

    let config = SessionSurfaceConfig {
        enabled: true,
        bind: "127.0.0.1".to_string(),
        // 0 asks the OS for a free port; the server reports what it bound.
        port: 0,
        allowed_origins: vec![],
        api_key: Some("secret".to_string()),
    };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = crate::session::surface::start_server(&config, agent, ctx, Some(ready_tx)).await;
    });

    let bound = tokio::time::timeout(Duration::from_secs(15), ready_rx)
        .await
        .expect("the server signals readiness within 15s")
        .expect("the ready channel stays open")
        .expect("the surface binds");

    // AC: assert on the address the socket actually bound, not the one the
    // config asked for, so `port = 0` cannot hide a non-loopback listener.
    assert!(
        bound.ip().is_loopback(),
        "the surface must listen on loopback, bound {bound}"
    );
    assert_ne!(bound.port(), 0, "the OS assigned a real port: {bound}");

    let base = format!("http://{bound}");
    let client = reqwest::Client::new();
    let submit_url = format!("{base}/surface/sessions/{}/submit", session.id);
    let body = serde_json::json!({ "request_id": "e2e-req-1", "input": "ping" });

    // ── one request id, submitted twice ─────────────────────────────────────
    let first: serde_json::Value = client
        .post(&submit_url)
        .header("authorization", "Bearer secret")
        .json(&body)
        .send()
        .await
        .expect("first submit")
        .json()
        .await
        .expect("first submit body");
    assert_eq!(first["created"], serde_json::json!(true), "first: {first}");

    let second: serde_json::Value = client
        .post(&submit_url)
        .header("authorization", "Bearer secret")
        .json(&body)
        .send()
        .await
        .expect("second submit")
        .json()
        .await
        .expect("second submit body");
    assert_eq!(
        second["created"],
        serde_json::json!(false),
        "a retry of one request id must not claim a second run: {second}"
    );
    assert_eq!(
        second["request_id"], first["request_id"],
        "the retry reads back the original row"
    );

    // ── stream to completion ────────────────────────────────────────────────
    let mut stream = client
        .get(format!("{base}/surface/sessions/{}/events", session.id))
        .header("authorization", "Bearer secret")
        .send()
        .await
        .expect("events response");
    assert_eq!(stream.status().as_u16(), 200, "the stream opens");

    let mut seen = String::new();
    let mut saw_done = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(15), stream.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains("event: done") {
                    saw_done = true;
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => panic!("the event stream failed: {e}"),
            Err(_) => panic!("the event stream went quiet for 15s; saw:\n{seen}"),
        }
    }
    assert!(
        saw_done,
        "the stream ends with a done event once the run is terminal; saw:\n{seen}"
    );
    assert!(
        seen.contains("event: submission"),
        "the stream reports submission state; saw:\n{seen}"
    );
    assert!(
        seen.contains("event: message"),
        "the stream reports committed transcript rows; saw:\n{seen}"
    );

    // ── the transcript must be exactly what the DB holds ────────────────────
    let api_rows: Vec<serde_json::Value> = client
        .get(format!("{base}/surface/sessions/{}/transcript", session.id))
        .header("authorization", "Bearer secret")
        .send()
        .await
        .expect("transcript response")
        .json()
        .await
        .expect("transcript body");
    let db_rows = MessageRepository::new(pool.clone())
        .list_by_session(session.id)
        .await
        .expect("read the transcript from the db");

    assert_eq!(
        api_rows.len(),
        db_rows.len(),
        "the endpoint and the DB must agree on how many rows exist"
    );
    for (api, db) in api_rows.iter().zip(db_rows.iter()) {
        assert_eq!(api["id"], serde_json::json!(db.id.to_string()));
        assert_eq!(api["role"], serde_json::json!(db.role.clone()));
        assert_eq!(api["sequence"], serde_json::json!(db.sequence));
    }
    assert!(
        api_rows
            .iter()
            .any(|row| row["id"] == serde_json::json!(seeded.id.to_string())),
        "the seeded row is part of the transcript: {api_rows:?}"
    );

    // ── one request id, one submission row ──────────────────────────────────
    let submissions = SubmissionRepository::new(pool.clone())
        .list_for_session(&session.id.to_string())
        .await
        .expect("list submissions");
    assert_eq!(
        submissions.len(),
        1,
        "two submits of one request id leave exactly one row: {submissions:?}"
    );
}

// ── FR-004: the submit path is idempotent on requestId ─────────────────────
//
// Milestone 2 part (c) shipped the storage contract with tests but had no
// production caller. This handler is that caller, so the idempotency claim is
// only real if it holds at the HTTP boundary and not just in the repository.

fn submit_request(session: &str, request_id: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/surface/sessions/{session}/submit"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "request_id": request_id,
                "input": "hello from the test",
                "model": null,
            })
            .to_string(),
        ))
        .expect("request")
}

/// `created` is the flag the handler uses to decide whether to spawn a run, so
/// reading it is how a test counts runs without watching a task spawn.
async fn created_flag(response: axum::response::Response) -> bool {
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    json["created"].as_bool().expect("created is a bool")
}

async fn submission_rows(pool: crate::db::Pool, session: &str) -> usize {
    crate::db::repository::submission::SubmissionRepository::new(pool)
        .list_for_session(session)
        .await
        .expect("list submissions")
        .len()
}

#[tokio::test]
async fn a_replayed_request_id_returns_the_same_submission_and_owns_one_run() {
    let state = test_state_with_token(Some("secret")).await;
    let pool = state.pool.clone();
    let app = build_router(state, &[]);
    let session = uuid::Uuid::new_v4().to_string();

    let first = app
        .clone()
        .oneshot(submit_request(&session, "req-replay", "secret"))
        .await
        .expect("first response");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert!(
        created_flag(first).await,
        "the first submit is the claim that owns the run"
    );

    let second = app
        .oneshot(submit_request(&session, "req-replay", "secret"))
        .await
        .expect("second response");
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    assert!(
        !created_flag(second).await,
        "a replay must not own a second run: created=false is what stops the spawn"
    );

    assert_eq!(
        submission_rows(pool, &session).await,
        1,
        "one requestId is one row, however many times it is submitted"
    );
}

#[tokio::test]
async fn two_parallel_submits_of_one_request_id_produce_exactly_one_row() {
    let state = test_state_with_token(Some("secret")).await;
    let pool = state.pool.clone();
    let app = build_router(state, &[]);
    let session = uuid::Uuid::new_v4().to_string();

    let a = app
        .clone()
        .oneshot(submit_request(&session, "req-parallel", "secret"));
    let b = app
        .clone()
        .oneshot(submit_request(&session, "req-parallel", "secret"));
    let (a, b) = tokio::join!(a, b);
    let (a, b) = (a.expect("a response"), b.expect("b response"));
    assert_eq!(a.status(), StatusCode::ACCEPTED);
    assert_eq!(b.status(), StatusCode::ACCEPTED);

    let created = [created_flag(a).await, created_flag(b).await];
    assert_eq!(
        created.iter().filter(|c| **c).count(),
        1,
        "exactly one of the two racing submits may own the run: {created:?}"
    );

    assert_eq!(
        submission_rows(pool, &session).await,
        1,
        "two parallel submits of one requestId is still one row"
    );
}
