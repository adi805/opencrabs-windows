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
