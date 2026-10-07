//! #1957: the shared/group-session gate must cover the INTERNAL memory
//! surfaces, not just `scope="external"`.
//!
//! Before this fix the gate had exactly one non-test caller (the `"external"`
//! arm of `memory_search`), so a group chat could still read the owner's
//! personal context through `scope="brain"` (SOUL/USER/AGENTS/MEMORY.md),
//! through the DEFAULT `scope="memory"` (daily logs), through
//! `load_brain_file`, and through the per-turn MEMORY.md recall. These tests
//! pin the boundary on each tool surface, plus the private-session path that
//! must keep working.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::ToolResult;
use crate::brain::tools::load_brain_file::LoadBrainFileTool;
use crate::brain::tools::memory_search::MemorySearchTool;
use crate::memory::mark_session_shared;
use serde_json::json;
use uuid::Uuid;

/// The refusal text every gated surface must produce.
const GATE: &str = "not available in this shared/group session";

fn text(r: &ToolResult) -> String {
    format!("{}{}", r.output, r.error.clone().unwrap_or_default())
}

async fn search(session_id: Uuid, scope: Option<&str>) -> String {
    let mut input = json!({ "query": "anything" });
    if let Some(s) = scope {
        input["scope"] = json!(s);
    }
    let ctx = ToolExecutionContext::new(session_id);
    let r = MemorySearchTool.execute(input, &ctx).await.expect("tool");
    text(&r)
}

#[tokio::test]
async fn brain_scope_is_refused_in_a_shared_session() {
    let s = Uuid::new_v4();
    mark_session_shared(s);
    let out = search(s, Some("brain")).await;
    assert!(out.contains(GATE), "scope=brain must be gated: {out}");
}

#[tokio::test]
async fn the_default_scope_is_refused_in_a_shared_session() {
    // The default scope is "memory" (daily logs): the most sensitive of the
    // three, and completely ungated before this fix.
    let s = Uuid::new_v4();
    mark_session_shared(s);
    let out = search(s, None).await;
    assert!(out.contains(GATE), "the default scope must be gated: {out}");
}

#[tokio::test]
async fn all_scope_is_refused_in_a_shared_session() {
    // scope="all" merges brain + memory, so it is gated by the same rule
    // rather than leaking the two halves the external flag does not cover.
    let s = Uuid::new_v4();
    mark_session_shared(s);
    let out = search(s, Some("all")).await;
    assert!(out.contains(GATE), "scope=all must be gated: {out}");
}

#[tokio::test]
async fn a_private_session_is_not_gated() {
    // Unmarked session: the gate must NOT fire. Whatever happens next (the
    // store may be unavailable inside a test binary) the refusal text must be
    // absent, and that absence is the property under test.
    let out = search(Uuid::new_v4(), Some("brain")).await;
    assert!(
        !out.contains(GATE),
        "private session must not be gated: {out}"
    );
}

#[tokio::test]
async fn load_brain_file_is_refused_in_a_shared_session() {
    let s = Uuid::new_v4();
    mark_session_shared(s);
    let ctx = ToolExecutionContext::new(s);
    let r = LoadBrainFileTool
        .execute(json!({ "name": "MEMORY.md" }), &ctx)
        .await
        .expect("tool");
    let out = text(&r);
    assert!(out.contains(GATE), "load_brain_file must be gated: {out}");
    assert!(!r.success, "a gated call is not a success");
}

#[tokio::test]
async fn load_brain_file_is_not_gated_in_a_private_session() {
    // A name that cannot exist, so the private path never reads a real brain
    // file: the assertion is about the gate, not about the file.
    let ctx = ToolExecutionContext::new(Uuid::new_v4());
    let r = LoadBrainFileTool
        .execute(json!({ "name": "NO-SUCH-BRAIN-FILE.md" }), &ctx)
        .await
        .expect("tool");
    let out = text(&r);
    assert!(
        !out.contains(GATE),
        "a private session must not hit the gate: {out}"
    );
}

#[test]
fn the_gate_key_defaults_to_deny() {
    // The opt-in must not be reachable by accident: the compiled default is
    // deny, which is what makes the boundary a boundary.
    let cfg = crate::config::types::MemoryConfig::default();
    assert!(
        !cfg.internal_allowed_in_shared,
        "internal_allowed_in_shared must default to false"
    );
    assert!(
        !cfg.external_allowed_in_shared,
        "external_allowed_in_shared must stay false"
    );
}
