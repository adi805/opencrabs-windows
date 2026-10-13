//! Tests for the Mission Control inbox service.
//!
//! Uses an explicit `ProposalsStore` rooted at a tmpdir so the tests
//! never touch the user's real `~/.opencrabs/rsi/` inbox.

use crate::brain::commands::UserCommand;
use crate::brain::mission_control::{McInboxKind, inbox_service};
use crate::brain::rsi_proposals::ProposalsStore;
use crate::brain::tools::dynamic::tool::{DynamicToolDef, ExecutorType};
use tempfile::TempDir;

fn shell_tool_def(name: &str, command: &str) -> DynamicToolDef {
    DynamicToolDef {
        name: name.to_string(),
        description: format!("test tool {name}"),
        executor: ExecutorType::Shell,
        enabled: true,
        requires_approval: true,
        method: None,
        url: None,
        headers: Default::default(),
        timeout_secs: 30,
        command: Some(command.to_string()),
        params: Vec::new(),
        max_response_bytes: None,
        allow_private_network: false,
    }
}

fn http_tool_def(name: &str, url: &str) -> DynamicToolDef {
    DynamicToolDef {
        name: name.to_string(),
        description: format!("test tool {name}"),
        executor: ExecutorType::Http,
        enabled: true,
        requires_approval: false,
        method: Some("POST".to_string()),
        url: Some(url.to_string()),
        headers: Default::default(),
        timeout_secs: 10,
        command: None,
        params: Vec::new(),
        max_response_bytes: None,
        allow_private_network: false,
    }
}

fn user_cmd(name: &str, prompt: &str) -> UserCommand {
    UserCommand {
        name: name.to_string(),
        description: String::new(),
        action: "prompt".to_string(),
        prompt: prompt.to_string(),
    }
}

#[test]
fn empty_inbox_returns_empty_list() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    assert!(inbox_service::list_with_store(&store).is_empty());
}

#[test]
fn shell_tool_proposal_renders_with_shell_summary() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_tool_proposal(
            "rsi-autonomous",
            "12 manual gh calls in last week",
            shell_tool_def("gh_issue_list", "gh issue list"),
        )
        .unwrap();

    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "gh_issue_list");
    assert_eq!(items[0].kind, McInboxKind::ProposedTool);
    assert_eq!(items[0].source, "rsi-autonomous");
    assert!(
        items[0].summary.starts_with("shell: "),
        "shell tools should prefix the summary with 'shell:': got {:?}",
        items[0].summary
    );
    assert!(items[0].summary.contains("gh issue list"));
}

#[test]
fn http_tool_proposal_renders_with_method_and_url() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_tool_proposal(
            "rsi-autonomous",
            "frequent webhook posts",
            http_tool_def("post_status", "https://hooks.example.com/status"),
        )
        .unwrap();
    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 1);
    assert!(
        items[0].summary.starts_with("POST "),
        "http tools should show METHOD URL: got {:?}",
        items[0].summary
    );
    assert!(items[0].summary.contains("hooks.example.com/status"));
}

#[test]
fn command_proposal_uses_prompt_body_as_summary() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_command_proposal(
            "rsi-autonomous",
            "user typed /deploy 5 times this week",
            user_cmd("/deploy", "Deploy the service to staging"),
        )
        .unwrap();
    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "/deploy");
    assert_eq!(items[0].kind, McInboxKind::ProposedCommand);
    assert_eq!(items[0].summary, "Deploy the service to staging");
}

#[test]
fn mixed_tool_and_command_proposals_appear_in_one_list() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_tool_proposal("rsi-autonomous", "ev1", shell_tool_def("a_tool", "echo a"))
        .unwrap();
    store
        .add_command_proposal("rsi-autonomous", "ev2", user_cmd("/b_cmd", "run b"))
        .unwrap();
    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 2);
    let kinds: Vec<McInboxKind> = items.iter().map(|i| i.kind).collect();
    assert!(kinds.contains(&McInboxKind::ProposedTool));
    assert!(kinds.contains(&McInboxKind::ProposedCommand));
}

#[test]
fn newer_items_sort_first() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    // Add the older one first; it carries an older timestamp.
    store
        .add_tool_proposal(
            "rsi-autonomous",
            "old",
            shell_tool_def("old_tool", "echo old"),
        )
        .unwrap();
    // Add a newer proposal — `add_tool_proposal` deduplicates by name,
    // so use a different slug to keep both.
    std::thread::sleep(std::time::Duration::from_millis(20));
    store
        .add_tool_proposal(
            "rsi-autonomous",
            "new",
            shell_tool_def("new_tool", "echo new"),
        )
        .unwrap();
    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 2);
    // Newest first.
    assert_eq!(items[0].label, "new_tool");
    assert_eq!(items[1].label, "old_tool");
}
