use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::session::*;
use tokio;

#[tokio::test]
async fn test_session_crud() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    // Create
    let session = Session::new(
        Some("Test Session".to_string()),
        Some("claude-sonnet-4-5".to_string()),
        Some("anthropic".to_string()),
    );
    repo.create(&session)
        .await
        .expect("Failed to create session");

    // Read
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    assert!(found.is_some());
    assert_eq!(
        found.as_ref().unwrap().title,
        Some("Test Session".to_string())
    );

    // Update
    let mut updated_session = session.clone();
    updated_session.title = Some("Updated Title".to_string());
    repo.update(&updated_session)
        .await
        .expect("Failed to update session");

    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    assert_eq!(found.unwrap().title, Some("Updated Title".to_string()));

    // Delete (soft-delete: row preserved with archived_at set)
    repo.delete(session.id)
        .await
        .expect("Failed to delete session");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    let found = found.expect("Soft-deleted session should still be findable by ID");
    assert!(
        found.archived_at.is_some(),
        "Soft-deleted session should have archived_at set"
    );
}

#[tokio::test]
async fn test_session_archive() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    let session = Session::new(Some("Test".to_string()), Some("model".to_string()), None);
    repo.create(&session)
        .await
        .expect("Failed to create session");

    // Archive
    repo.archive(session.id).await.expect("Failed to archive");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find")
        .unwrap();
    assert!(found.is_archived());

    // Unarchive
    repo.unarchive(session.id)
        .await
        .expect("Failed to unarchive");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find")
        .unwrap();
    assert!(!found.is_archived());
}

/// `update()` is not a whole-row write, and the two fields it excludes are a
/// contract rather than an oversight (F-DB-05).
///
/// A caller that mutates `sharing` or `channel_chat_key` in memory and saves
/// gets no error and no write, which is exactly the shape of bug that reads as
/// "the setting did not stick". The doc comment on `update` states the
/// contract; this test is what keeps the statement and the doc from drifting
/// apart, since nothing else would notice a column being added or dropped.
#[tokio::test]
async fn update_is_not_a_whole_row_write() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    let mut session = Session::new(
        Some("Original".to_string()),
        Some("model".to_string()),
        Some("provider".to_string()),
    );
    session.sharing = Some("personal".to_string());
    // The only INSERT that carries a chat key, so the fixture holds one.
    let session = repo
        .insert_or_resolve_channel(&session, "slack:C_ORIGINAL")
        .await
        .expect("insert channel session");

    let mut mutated = session.clone();
    mutated.title = Some("Renamed".to_string());
    mutated.token_count = 123;
    mutated.sharing = Some("shared".to_string());
    mutated.channel_chat_key = Some("slack:C_MOVED".to_string());
    repo.update(&mutated).await.expect("update session");

    let after = repo
        .find_by_id(session.id)
        .await
        .expect("find session")
        .expect("row still exists");

    // The columns update() owns did change.
    assert_eq!(after.title.as_deref(), Some("Renamed"));
    assert_eq!(after.token_count, 123);

    // The two it excludes did not, whatever the in-memory copy said.
    assert_eq!(
        after.sharing.as_deref(),
        Some("personal"),
        "update() must not write sharing; set_sharing is the only writer (#217)"
    );
    assert_eq!(
        after.channel_chat_key.as_deref(),
        Some("slack:C_ORIGINAL"),
        "update() must not move a chat key; insert_or_resolve_channel owns it (#1721)"
    );

    // And the dedicated writer is what moves sharing, so the exclusion above is
    // a routing rule rather than a field nothing can change.
    repo.set_sharing(session.id, "shared")
        .await
        .expect("set sharing");
    assert_eq!(
        repo.sharing_of(session.id)
            .await
            .expect("read sharing")
            .as_deref(),
        Some("shared")
    );
}
