//! browser_act (multi_act, #1188) — pure-core tests. No Chrome needed:
//! parsing/validation, cap enforcement, prefix reporting, and pre-flight
//! JS shape are all pure functions. The Chrome-requiring login-fixture
//! e2e lives at the bottom, `#[ignore]`'d per house convention
//! (see browser_e2e_test.rs for the rationale).

#![cfg(feature = "browser")]

use crate::brain::tools::browser::{
    Act, build_pre_flight_js_with, format_prefix_report, parse_actions, resolve_upload_paths,
};
use serde_json::json;

fn actions(v: serde_json::Value) -> serde_json::Value {
    json!({ "actions": v })
}

#[test]
fn parses_all_five_action_types() {
    let input = actions(json!([
        {"type": "click",  "selector": "#submit"},
        {"type": "fill",   "selector": "input[name=user]", "text": "adolfo"},
        {"type": "press",  "key": "Enter"},
        {"type": "select", "selector": "#country", "value": "PT"},
        {"type": "wait",   "ms": 500},
    ]));
    let acts = parse_actions(&input).expect("valid batch parses");
    assert_eq!(acts.len(), 5);
    assert_eq!(
        acts[0],
        Act::Click {
            selector: "#submit".into()
        }
    );
    assert_eq!(
        acts[1],
        Act::Fill {
            selector: "input[name=user]".into(),
            text: "adolfo".into()
        }
    );
    assert_eq!(
        acts[2],
        Act::Press {
            key: "Enter".into()
        }
    );
    assert_eq!(
        acts[3],
        Act::Select {
            selector: "#country".into(),
            value: "PT".into()
        }
    );
    assert_eq!(acts[4], Act::Wait { ms: 500 });
}

#[test]
fn cap_of_ten_is_enforced_up_front() {
    let eleven: Vec<_> = (0..11)
        .map(|i| json!({"type": "press", "key": format!("Key{i}")}))
        .collect();
    let err = parse_actions(&actions(json!(eleven))).unwrap_err();
    assert!(err.contains("Too many actions: 11"), "got: {err}");
    assert!(err.contains("cap is 10"), "got: {err}");

    // Exactly ten passes — the cap is inclusive.
    let ten: Vec<_> = (0..10)
        .map(|i| json!({"type": "press", "key": format!("Key{i}")}))
        .collect();
    assert_eq!(parse_actions(&actions(json!(ten))).unwrap().len(), 10);
}

#[test]
fn unknown_type_and_missing_fields_are_rejected_with_action_numbers() {
    let err = parse_actions(&actions(json!([
        {"type": "press", "key": "Enter"},
        {"type": "drag", "selector": "#x"},
    ])))
    .unwrap_err();
    assert!(err.contains("action 2"), "got: {err}");
    assert!(err.contains("unknown type 'drag'"), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "fill", "selector": "#u"}]))).unwrap_err();
    assert!(err.contains("action 1"), "got: {err}");
    assert!(err.contains("\"text\""), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "click"}]))).unwrap_err();
    assert!(err.contains("\"selector\""), "got: {err}");
}

#[test]
fn wait_is_bounded_per_action() {
    let err = parse_actions(&actions(json!([{"type": "wait", "ms": 60000}]))).unwrap_err();
    assert!(err.contains("exceeds the per-action cap"), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "wait", "ms": 0}]))).unwrap_err();
    assert!(err.contains("positive"), "got: {err}");
}

#[test]
fn empty_or_missing_actions_array_is_rejected() {
    assert!(parse_actions(&json!({})).is_err());
    assert!(parse_actions(&actions(json!([]))).is_err());
}

#[test]
fn prefix_report_names_completed_prefix_and_holds_remaining() {
    let failed = Act::Click {
        selector: "#gone".into(),
    };
    // Two done, third failed → "actions 1-2 OK; action 3 …"
    let r = format_prefix_report(2, &failed, "element no longer present");
    assert!(r.starts_with("actions 1-2 OK;"), "got: {r}");
    assert!(r.contains("action 3 (click #gone) failed"), "got: {r}");
    assert!(r.contains("NOT run"), "got: {r}");

    // Nothing done → whole call failed cleanly.
    let r = format_prefix_report(0, &failed, "not found");
    assert!(r.contains("action 1 (click #gone) failed"), "got: {r}");
    assert!(r.contains("No actions executed"), "got: {r}");
}

#[test]
fn pre_flight_js_embeds_all_selectors_and_all_three_shapes() {
    let sels = vec![
        "#submit".to_string(),
        "text=Sign in".to_string(),
        "xpath=//button[1]".to_string(),
    ];
    let js = build_pre_flight_js_with(&sels, &[true, true, true]);
    // All three selectors ride in as a JSON array.
    assert!(js.contains("\"#submit\""), "js: {js}");
    assert!(js.contains(r#""text=Sign in""#), "js: {js}");
    assert!(js.contains(r#""xpath=//button[1]""#), "js: {js}");
    // All three resolution strategies present.
    // Pre-flight must resolve exactly what execution will resolve: css
    // and text pierce open shadow roots, xpath stays main-document
    // because the XPath spec has no shadow boundary.
    assert!(js.contains("text=") && js.contains("__ocWalk()"));
    assert!(js.contains("xpath=") && js.contains("document.evaluate"));
    assert!(js.contains("__ocQueryOne(s)"));
    // Visibility gating is part of resolution (zero-size rects fail).
    assert!(js.contains("getBoundingClientRect"));
    // Per-selector result objects carry index + ok + reason.
    assert!(js.contains("results.push"));
}

// ── upload (#2112) ────────────────────────────────────────────────────────

#[test]
fn upload_parses_with_absolute_paths_and_labels_the_file_count() {
    let acts = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": ["/tmp/a.mp4", "/tmp/b.png"]},
    ])))
    .expect("upload parses");
    assert_eq!(
        acts[0],
        Act::Upload {
            selector: "input[type=file]".into(),
            files: vec!["/tmp/a.mp4".into(), "/tmp/b.png".into()],
        }
    );
    assert_eq!(acts[0].label(), "upload input[type=file] (2 files)");
    assert_eq!(acts[0].selector(), Some("input[type=file]"));
}

#[test]
fn upload_requires_a_selector_and_a_non_empty_files_array() {
    let err = parse_actions(&actions(json!([
        {"type": "upload", "files": ["/tmp/a"]},
    ])))
    .unwrap_err();
    assert!(err.contains("action 1"), "got: {err}");
    assert!(err.contains("\"selector\""), "got: {err}");

    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]"},
    ])))
    .unwrap_err();
    assert!(err.contains("non-empty \"files\" array"), "got: {err}");

    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": []},
    ])))
    .unwrap_err();
    assert!(err.contains("non-empty \"files\" array"), "got: {err}");

    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": ["/tmp/a", 7]},
    ])))
    .unwrap_err();
    assert!(err.contains("non-empty string"), "got: {err}");
}

#[test]
fn upload_rejects_a_relative_path_before_any_browser_work() {
    // A relative path resolves against the BROWSER process's cwd, so the same
    // action would attach different files depending on how it was launched.
    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": ["secrets/id_rsa"]},
    ])))
    .unwrap_err();
    assert!(err.contains("is relative"), "got: {err}");
    assert!(err.contains("absolute path"), "got: {err}");
}

#[test]
fn resolve_upload_paths_verifies_existence_and_regular_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"png").expect("write");

    let mut acts = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]",
         "files": [file.to_string_lossy()]},
    ])))
    .expect("parses");
    resolve_upload_paths(&mut acts).expect("existing regular file resolves");
    match &acts[0] {
        Act::Upload { files, .. } => {
            // Canonicalised: what the browser used is what we echo back.
            let expected = std::fs::canonicalize(&file)
                .unwrap()
                .to_string_lossy()
                .to_string();
            assert_eq!(files[0], expected);
        }
        other => panic!("expected upload, got {other:?}"),
    }

    // Missing file.
    let mut acts = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]",
         "files": [dir.path().join("nope.mp4").to_string_lossy()]},
    ])))
    .expect("parses");
    let err = resolve_upload_paths(&mut acts).unwrap_err();
    assert!(err.contains("is not readable"), "got: {err}");
    assert!(err.contains("NOTHING executed"), "got: {err}");

    // Directory: exists, but is not a regular file.
    let mut acts = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]",
         "files": [dir.path().to_string_lossy()]},
    ])))
    .expect("parses");
    let err = resolve_upload_paths(&mut acts).unwrap_err();
    assert!(err.contains("is not a regular file"), "got: {err}");
}

#[test]
fn resolve_upload_paths_leaves_other_actions_alone() {
    let mut acts = parse_actions(&actions(json!([
        {"type": "click", "selector": "#go"},
        {"type": "wait", "ms": 10},
    ])))
    .expect("parses");
    resolve_upload_paths(&mut acts).expect("no upload actions, nothing to check");
    assert_eq!(
        acts[0],
        Act::Click {
            selector: "#go".into()
        }
    );
}

#[test]
fn upload_targets_are_exempt_from_the_visibility_requirement() {
    // A file input is usually hidden behind a styled button, so demanding a
    // non-zero rect would reject the normal case. Presence is still required.
    let sels = vec!["#submit".to_string(), "input[type=file]".to_string()];
    let js = build_pre_flight_js_with(&sels, &[true, false]);
    assert!(js.contains("var vis = [true,false];"), "js: {js}");
    assert!(js.contains("var needVis = vis[i] !== false;"), "js: {js}");
    assert!(js.contains("!needVis ||"), "js: {js}");
    assert!(js.contains(r#""not found""#), "js: {js}");

    // All-visible is what the plain batch path asks for.
    let js = build_pre_flight_js_with(&sels, &[true, true]);
    assert!(js.contains("var vis = [true,true];"), "js: {js}");
}

/// Login-form fixture acceptance (#1188): a full fill+fill+press
/// sequence completes in ONE call with a success result and screenshot.
/// `#[ignore]` — launches real Chrome (house convention, see header).
#[tokio::test]
#[ignore = "launches real Chrome — opt-in via `cargo test -- --ignored browser_act`"]
async fn login_form_completes_in_one_call() {
    use crate::brain::tools::browser::{BrowserActTool, BrowserManager};
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use std::sync::Arc;
    use uuid::Uuid;

    let mgr = Arc::new(BrowserManager::new(Default::default()));
    let act = BrowserActTool::new(mgr.clone());
    let ctx = ToolExecutionContext::new(Uuid::new_v4());

    // The tried-and-true public login fixture used across browser e2e.
    let nav = crate::brain::tools::browser::BrowserNavigateTool::new(mgr.clone());
    let _ = Tool::execute(
        &nav,
        json!({"url": "https://the-internet.herokuapp.com/login"}),
        &ctx,
    )
    .await;

    let out = Tool::execute(
        &act,
        actions(json!([
            {"type": "fill", "selector": "#username", "text": "tomsmith"},
            {"type": "fill", "selector": "#password", "text": "SuperSecretPassword!"},
            {"type": "press", "key": "Enter"},
        ])),
        &ctx,
    )
    .await
    .expect("execute ok");

    assert!(out.success, "batch should succeed: {:?}", out.error);
    assert!(
        out.output.contains("Executed 3 actions"),
        "got: {}",
        out.output
    );
    let close = crate::brain::tools::browser::BrowserCloseTool::new(mgr);
    let _ = close.execute(serde_json::json!({}), &ctx).await;
}
