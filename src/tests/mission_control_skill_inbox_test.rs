//! Tests for skill proposals surfacing in Mission Control's Inbox
//! and the SKILL.md write that happens on `a` (apply).
//!
//! Regression context: commit 8c9d959b added the `skill` proposal
//! kind to `rsi_propose`, but `inbox_service::list_with_store` was
//! never taught about it. So an agent could file a skill via
//! `rsi_propose kind=skill`, the file landed in
//! `~/.opencrabs/rsi/proposed_skills.toml`, but Mission Control's
//! Inbox panel showed nothing. Users had no runtime path to apply
//! / reject those proposals.

use crate::brain::mission_control::{McInboxKind, inbox_service};
use crate::brain::rsi_proposals::{ProposalsStore, ProposedSkill};
use crate::brain::tools::registry::ToolRegistry;
use crate::brain::tools::rsi_proposals::RsiProposalsTool;
use std::sync::Arc;
use tempfile::TempDir;

fn sample_skill() -> ProposedSkill {
    ProposedSkill {
        name: "github_release_pipeline".to_string(),
        description: "Run the standard release sequence: branch check, changelog, tag, publish."
            .to_string(),
        body: "# GitHub Release Pipeline\n\n\
               1. Confirm working tree clean.\n\
               2. Draft CHANGELOG entry.\n\
               3. Bump version in Cargo.toml.\n\
               4. Tag + push."
            .to_string(),
    }
}

#[test]
fn inbox_list_surfaces_skill_proposals() {
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_skill_proposal("rsi-autonomous", "saw 14 release sequences", sample_skill())
        .unwrap();

    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, McInboxKind::ProposedSkill);
    assert_eq!(items[0].label, "github_release_pipeline");
    assert!(items[0].summary.contains("release sequence"));
    assert_eq!(items[0].source, "rsi-autonomous");
}

#[test]
fn inbox_list_sorts_tool_command_skill_by_creation_time() {
    // Mixed inbox: ensure skills appear in the same chronological
    // sort as tools + commands (newest first). Without this the
    // skill section could be hidden below older tool entries that
    // the user already saw.
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());

    use crate::brain::commands::UserCommand;
    use crate::brain::tools::dynamic::tool::{DynamicToolDef, ExecutorType};

    store
        .add_tool_proposal(
            "rsi",
            "evidence",
            DynamicToolDef {
                name: "gh_issue_list".to_string(),
                description: "List GitHub issues".to_string(),
                executor: ExecutorType::Shell,
                enabled: true,
                requires_approval: true,
                method: None,
                url: None,
                headers: Default::default(),
                timeout_secs: 30,
                command: Some("gh issue list".to_string()),
                params: Vec::new(),

                max_response_bytes: None,
                allow_private_network: false,
            },
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    store
        .add_command_proposal(
            "rsi",
            "evidence",
            UserCommand {
                name: "/standup".to_string(),
                description: "3-bullet summary".to_string(),
                action: "prompt".to_string(),
                prompt: "Summarise yesterday".to_string(),
            },
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();

    let items = inbox_service::list_with_store(&store);
    assert_eq!(items.len(), 3);
    // Newest first → skill, then command, then tool.
    assert_eq!(items[0].kind, McInboxKind::ProposedSkill);
    assert_eq!(items[1].kind, McInboxKind::ProposedCommand);
    assert_eq!(items[2].kind, McInboxKind::ProposedTool);
}

#[test]
fn apply_skill_writes_skill_md_with_frontmatter() {
    let brain_dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    let id = store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();

    let tools_path = brain_dir.path().join("tools.toml");
    let registry = Arc::new(ToolRegistry::new());
    let tool = RsiProposalsTool::new(registry, tools_path, brain_dir.path().to_path_buf());

    let result = tool.apply_skill(&id).expect("apply succeeds");
    assert!(result.contains("Installed skill 'github_release_pipeline'"));
    assert!(result.contains("SKILL.md"));

    let skill_path = brain_dir
        .path()
        .join("skills")
        .join("github_release_pipeline")
        .join("SKILL.md");
    assert!(
        skill_path.exists(),
        "SKILL.md should be written at {}",
        skill_path.display()
    );
    let contents = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        contents.starts_with("---\n"),
        "must start with YAML frontmatter: {contents}"
    );
    assert!(
        contents.contains("name: github_release_pipeline"),
        "frontmatter must carry the slug: {contents}"
    );
    assert!(
        contents.contains("description: Run the standard release sequence"),
        "frontmatter must carry the description: {contents}"
    );
    assert!(
        contents.contains("# GitHub Release Pipeline"),
        "body must follow frontmatter: {contents}"
    );
    assert!(
        contents.contains("1. Confirm working tree clean."),
        "full body must be preserved: {contents}"
    );
}

#[test]
fn apply_skill_refuses_to_overwrite_existing_skill() {
    // A user who manually wrote a skill at `<name>/SKILL.md` should
    // never have it silently replaced by an RSI proposal that
    // picked the same slug. The apply must error out so the user
    // can reject the proposal or remove the existing skill first.
    let brain_dir = TempDir::new().unwrap();
    let skills_dir = brain_dir
        .path()
        .join("skills")
        .join("github_release_pipeline");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("SKILL.md"), "existing content").unwrap();

    let store = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    let id = store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();

    let tool = RsiProposalsTool::new(
        Arc::new(ToolRegistry::new()),
        brain_dir.path().join("tools.toml"),
        brain_dir.path().to_path_buf(),
    );
    let result = tool.apply_skill(&id);
    assert!(result.is_err(), "must refuse to overwrite");
    let err = result.unwrap_err();
    assert!(err.contains("already exists"), "got: {err}");

    // The existing content stays intact.
    let after = std::fs::read_to_string(skills_dir.join("SKILL.md")).unwrap();
    assert_eq!(after, "existing content");
}

#[test]
fn apply_skill_removes_proposal_from_inbox() {
    let brain_dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    let id = store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();
    assert_eq!(store.list_skill_proposals().len(), 1);

    let tool = RsiProposalsTool::new(
        Arc::new(ToolRegistry::new()),
        brain_dir.path().join("tools.toml"),
        brain_dir.path().to_path_buf(),
    );
    tool.apply_skill(&id).expect("apply succeeds");

    let store_after = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    assert!(
        store_after.list_skill_proposals().is_empty(),
        "applied proposal must be removed from inbox"
    );
}

#[test]
fn apply_skill_archives_to_daily_log() {
    let brain_dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    let id = store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();

    let tool = RsiProposalsTool::new(
        Arc::new(ToolRegistry::new()),
        brain_dir.path().join("tools.toml"),
        brain_dir.path().to_path_buf(),
    );
    tool.apply_skill(&id).expect("apply succeeds");

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let archive = brain_dir
        .path()
        .join("rsi")
        .join("applied")
        .join(format!("{date}-skills.toml"));
    assert!(
        archive.exists(),
        "applied archive must be written at {}",
        archive.display()
    );
    let contents = std::fs::read_to_string(&archive).unwrap();
    assert!(contents.contains("github_release_pipeline"));
}

#[test]
fn reject_skill_archives_with_reason() {
    let brain_dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    let id = store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();

    let tool = RsiProposalsTool::new(
        Arc::new(ToolRegistry::new()),
        brain_dir.path().join("tools.toml"),
        brain_dir.path().to_path_buf(),
    );
    let result = tool.reject(&id, Some("manual release process is preferred"));
    assert!(result.is_ok(), "reject should succeed; got: {result:?}");
    assert!(result.unwrap().contains("Rejected skill proposal"));

    let store_after = ProposalsStore::with_dir(brain_dir.path().join("rsi"));
    assert!(store_after.list_skill_proposals().is_empty());

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let archive = brain_dir
        .path()
        .join("rsi")
        .join("rejected")
        .join(format!("{date}-skills.toml"));
    assert!(archive.exists());
    let contents = std::fs::read_to_string(&archive).unwrap();
    assert!(contents.contains("manual release process"));
}

#[test]
fn pending_count_includes_skill_for_session_banner() {
    // The session-start banner reads pending_count to decide
    // whether to surface its "you have N RSI proposals" hint.
    // Confirm skills contribute so a session that has ONLY skill
    // proposals still triggers the banner.
    let dir = TempDir::new().unwrap();
    let store = ProposalsStore::with_dir(dir.path().to_path_buf());
    store
        .add_skill_proposal("rsi", "evidence", sample_skill())
        .unwrap();
    assert_eq!(store.pending_count(), 1);
}
