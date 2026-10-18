//! #2121: `content_by_platform_message_id` must return the row that can
//! identify the artifact.
//!
//! A media message is persisted twice under one platform id: the main store
//! writes `<caption>\n[file: <abs path>]`, and the late "log ALL processed
//! messages" block wrote a pathless `[photo] <caption>` twin one second later.
//! `ORDER BY created_at DESC LIMIT 1` therefore handed back the twin, which is
//! the one row that cannot say which file the user replied to. The fix orders
//! by "carries a path" first, then recency, and stops writing the twin.

use crate::db::Database;
use crate::db::models::ChannelMessage;
use crate::db::repository::channel_message::*;

const CHAT: &str = "-1004367145331";
const PMID: &str = "91";

async fn repo() -> (Database, ChannelMessageRepository) {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = ChannelMessageRepository::new(db.pool().clone());
    (db, repo)
}

/// Insert a row and pin its `created_at`, so the ordering under test is the
/// content preference and not the insertion order.
async fn insert_at(db: &Database, repo: &ChannelMessageRepository, content: &str, created_at: i64) {
    let msg = ChannelMessage::new(
        "telegram".into(),
        CHAT.into(),
        Some("Test Group".into()),
        "42".into(),
        "Adi".into(),
        content.into(),
        "photo".into(),
        Some(PMID.into()),
    );
    repo.insert(&msg).await.expect("Failed to insert");
    let id = msg.id.to_string();
    db.pool()
        .get()
        .await
        .expect("connection")
        .interact(move |conn| {
            conn.execute(
                "UPDATE channel_messages SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, id],
            )
        })
        .await
        .expect("interact")
        .expect("Failed to pin created_at");
}

#[tokio::test]
async fn the_row_carrying_the_path_wins_over_the_newer_pathless_twin() {
    let (db, repo) = repo().await;
    insert_at(
        &db,
        &repo,
        "Ini wajah gua bro\n[file: /root/.opencrabs/channel_attachments/telegram/photo_91.jpg]",
        1792249530,
    )
    .await;
    insert_at(&db, &repo, "[photo] Ini wajah gua bro", 1792249531).await;

    let content = repo
        .content_by_platform_message_id("telegram", CHAT, PMID)
        .await
        .expect("lookup")
        .expect("a row");
    assert!(
        content.contains("[file: "),
        "the lookup must prefer the row that names the file, got: {content}"
    );
}

#[tokio::test]
async fn a_captionless_photo_no_longer_resolves_to_the_bare_marker() {
    let (db, repo) = repo().await;
    insert_at(
        &db,
        &repo,
        "[file: /root/.opencrabs/channel_attachments/telegram/photo_91.jpg]",
        1792249530,
    )
    .await;
    insert_at(&db, &repo, "[photo] ", 1792249531).await;

    let content = repo
        .content_by_platform_message_id("telegram", CHAT, PMID)
        .await
        .expect("lookup")
        .expect("a row");
    assert_ne!(content.trim(), "[photo]", "the bare marker is not content");
    assert!(content.contains("photo_91.jpg"), "got: {content}");
}

#[tokio::test]
async fn a_single_pathless_row_is_still_returned() {
    // The preference must not turn a genuine single row into a miss: older
    // rows carry no attachment at all.
    let (db, repo) = repo().await;
    insert_at(&db, &repo, "halo bro", 1792249530).await;

    assert_eq!(
        repo.content_by_platform_message_id("telegram", CHAT, PMID)
            .await
            .expect("lookup"),
        Some("halo bro".to_string())
    );
}

#[tokio::test]
async fn recency_still_decides_between_two_rows_with_the_same_shape() {
    let (db, repo) = repo().await;
    insert_at(&db, &repo, "[photo] first", 1792249530).await;
    insert_at(&db, &repo, "[photo] second", 1792249531).await;

    assert_eq!(
        repo.content_by_platform_message_id("telegram", CHAT, PMID)
            .await
            .expect("lookup"),
        Some("[photo] second".to_string()),
        "with no path on either row, the newest still wins"
    );
}

#[tokio::test]
async fn an_unknown_id_is_a_miss() {
    let (db, repo) = repo().await;
    insert_at(&db, &repo, "halo", 1792249530).await;

    assert_eq!(
        repo.content_by_platform_message_id("telegram", CHAT, "999")
            .await
            .expect("lookup"),
        None
    );
}
