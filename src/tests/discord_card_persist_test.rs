//! Durable Discord card state (#193): a tool-group bubble and a pager that
//! survive a restart.
//!
//! Which message carries a turn's tool group lived only in
//! `DiscordState::tool_groups`, a process-local map, and a paged answer's pages
//! only in `long_answers`. Both cards stay on screen across a restart, so the
//! process that came back held neither: pressing Expand answered "This status
//! bubble is too old to expand" on a card that was still sitting in the
//! channel, and pressing a pager arrow answered "That pager aged out".
//!
//! Both cards now write through to `discord_card_state`, told apart by a `kind`
//! column, and read back on a miss. The store is additive: with none wired the
//! in-memory path is unchanged, which the control test at the bottom pins.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::channels::background_work::SubagentCounts;
use crate::channels::discord::DiscordState;
use crate::channels::discord::long_answer::page_body;
use crate::channels::discord::tool_group::{
    EntrySnapshot, GroupEntry, GroupSnapshot, GroupState, SettledStatus, TOOL_GROUP_KIND,
    TurnOutcome, WAITING_ICON, render_content,
};
use crate::db::Database;
use crate::db::repository::{DiscordCardRepository, DiscordCardRow};
use serenity::model::id::ChannelId;
use std::time::{Duration, Instant};

/// A fresh in-memory database, migrated, with a repository factory so each
/// "process" gets its own handle to the SAME store — which is what makes the
/// restart meaningful.
async fn migrated_db() -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

fn store_for(db: &Database) -> DiscordCardRepository {
    DiscordCardRepository::new(db.pool().clone())
}

/// A `DiscordState` standing in for one process run: same database, empty maps.
async fn process(db: &Database) -> DiscordState {
    let state = DiscordState::new();
    state.set_card_store(store_for(db)).await;
    state
}

fn group(n: usize, done: bool) -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: (0..n)
            .map(|i| GroupEntry {
                name: format!("tool{i}"),
                context: format!(" (arg{i})"),
                status: if done { Some(true) } else { None },
            })
            .collect(),
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

fn settled_group(n: usize) -> GroupState {
    let mut g = group(n, true);
    g.settled = Some(SettledStatus::new(
        TurnOutcome::Finished,
        Duration::from_secs(1),
        None,
    ));
    g
}

/// The restart case: a later process must find the group, or the card on screen
/// can never be redrawn (#193).
#[tokio::test]
async fn a_tool_group_survives_a_restart() {
    let db = migrated_db().await;
    let before = process(&db).await;
    before.upsert_tool_group(7001, group(3, false)).await;

    let after = process(&db).await;
    let recovered = after
        .tool_group_snapshot(7001)
        .await
        .expect("the group must survive a restart");
    assert_eq!(recovered.entries.len(), 3);
    assert!(!recovered.expanded);
    assert!(recovered.settled.is_none(), "the turn was still running");
}

/// The press the issue is about: Expand on a card that predates the restart
/// must answer with the stored rendering, not "too old to expand".
#[tokio::test]
async fn the_expand_press_works_on_a_card_that_predates_the_restart() {
    let db = migrated_db().await;
    let before = process(&db).await;
    before.upsert_tool_group(7002, group(2, true)).await;

    let after = process(&db).await;
    let toggled = after
        .toggle_tool_group(7002)
        .await
        .expect("the press must be served, not answered with \"too old to expand\"");
    assert!(toggled.expanded);
    assert_eq!(
        toggled.entries.len(),
        2,
        "the stored rendering is what answers the press"
    );
    assert!(
        render_content(&toggled).contains("tool0"),
        "and it is the expanded rendering, so the rows are on screen"
    );
}

/// `Instant` is process-local, so the clock travels as an age: a card 4 minutes
/// into a turn must still read 4 minutes after the restart, not 0.
#[tokio::test]
async fn the_clock_does_not_restart() {
    let db = migrated_db().await;
    let before = process(&db).await;
    let mut started = group(2, false);
    started.started_at = Instant::now() - Duration::from_secs(240);
    started.last_activity_at = Instant::now() - Duration::from_secs(240);
    before.upsert_tool_group(7003, started).await;

    let after = process(&db).await;
    let snap = after
        .tool_group_snapshot(7003)
        .await
        .expect("survives")
        .to_snapshot();
    assert!(
        snap.started_secs_ago >= 240,
        "a card 4 minutes in must not reset to 0 across the restart; got {}s",
        snap.started_secs_ago
    );
    assert!(snap.started_secs_ago < 300, "and must not have run away");
}

/// A turn that had already settled still reads settled, with the elapsed it
/// froze at and the ctx budget line it carried.
#[tokio::test]
async fn a_settled_card_still_reads_settled() {
    let db = migrated_db().await;
    let before = process(&db).await;
    let mut ran = group(2, true);
    ran.started_at = Instant::now() - Duration::from_secs(300);
    before.upsert_tool_group(7004, ran).await;
    before
        .settle_tool_group(
            7004,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            Some("ctx: 12K/200K 6%".to_string()),
        )
        .await
        .expect("the group is retained");

    let after = process(&db).await;
    let recovered = after.tool_group_snapshot(7004).await.expect("survives");
    let settled = recovered
        .settled
        .clone()
        .expect("the settled line survives");
    assert_eq!(settled.outcome, TurnOutcome::Finished);
    let elapsed = settled.elapsed.as_secs();
    assert!(
        (300..360).contains(&elapsed),
        "the settled clock must still read the elapsed it froze at (~300s), not 0; got {elapsed}s"
    );
    assert_eq!(settled.ctx.as_deref(), Some("ctx: 12K/200K 6%"));
    assert!(
        render_content(&recovered).contains("Finished"),
        "the card comes back reading settled"
    );
}

/// The waiting override travels as the fact that produced it, not as a stored
/// icon: a `✅ Finished` turn that settled with detached work alive has to come
/// back reading `⏳ Waiting for 1 background task` (#1144/#1183).
#[tokio::test]
async fn the_waiting_override_survives() {
    let db = migrated_db().await;
    let before = process(&db).await;
    before.upsert_tool_group(7005, group(2, true)).await;
    before
        .settle_tool_group(
            7005,
            TurnOutcome::Finished,
            1,
            SubagentCounts::default(),
            None,
        )
        .await
        .expect("retained");

    let after = process(&db).await;
    let settled = after
        .tool_group_snapshot(7005)
        .await
        .expect("survives")
        .settled
        .expect("settled");
    assert_eq!(settled.icon, WAITING_ICON);
    assert_eq!(settled.verb, "Waiting for 1 background task");
}

/// Eviction stays honest across a reload: the cap forgetting a card must drop
/// its row too, or the next press resurrects a card the map deliberately let go.
#[tokio::test]
async fn an_evicted_card_is_not_resurrected() {
    let db = migrated_db().await;
    let before = process(&db).await;
    for i in 0..25u64 {
        before.upsert_tool_group(i, settled_group(2)).await;
    }

    let after = process(&db).await;
    assert!(
        after.tool_group_snapshot(0).await.is_none(),
        "the cap forgot this card, so its durable row must be gone with it"
    );
    assert!(
        after.tool_group_snapshot(24).await.is_some(),
        "the newest card is still retained"
    );
}

/// The pager's own restart case: the pages and the channel they were posted in
/// are durable, so a press after the restart is served the page it asked for.
#[tokio::test]
async fn a_pager_survives_a_restart() {
    let db = migrated_db().await;
    let pages = vec!["page one".to_string(), "page two".to_string()];
    let before = process(&db).await;
    before
        .store_long_answer(7006, ChannelId::new(4242), pages.clone())
        .await;

    let after = process(&db).await;
    let recovered = after
        .long_answer_pages(7006)
        .await
        .expect("a press after the restart must be served, not answered \"aged out\"");
    assert_eq!(recovered, pages);
    assert_eq!(
        page_body(&recovered, 1).as_deref(),
        Some("page two\n\n-# Page 2 of 2"),
        "and it renders the page the arrow named"
    );
}

/// One table serves both cards, so the `kind` column is what keeps a pager from
/// being read back as a tool group: the two snapshots are not interchangeable.
#[tokio::test]
async fn a_pager_is_not_read_back_as_a_tool_group() {
    let db = migrated_db().await;
    let before = process(&db).await;
    before
        .store_long_answer(7008, ChannelId::new(4242), vec!["page one".to_string()])
        .await;

    let after = process(&db).await;
    assert!(
        after.tool_group_snapshot(7008).await.is_none(),
        "the kind column is what keeps the two cards apart"
    );
    assert!(after.long_answer_pages(7008).await.is_some());
}

/// A card written by a build that knows an outcome this one does not cannot be
/// rendered honestly, so it must answer "too old" rather than invent a status
/// on the one card the user is watching.
#[tokio::test]
async fn an_unknown_stored_outcome_is_not_invented() {
    let db = migrated_db().await;
    let state = process(&db).await;
    let json = r#"{"entries":[],"notes":[],"expanded":false,"started_secs_ago":1,
        "activity_secs_ago":1,"settled":{"outcome":"exploded","elapsed_secs":2,
        "ctx":null,"verb":"Exploded","waiting":false},"live_ctx":null}"#;
    store_for(&db)
        .set(DiscordCardRow {
            message_id: 7007,
            kind: TOOL_GROUP_KIND.to_string(),
            state: json.to_string(),
        })
        .await
        .expect("the row is written");

    assert!(
        state.tool_group_snapshot(7007).await.is_none(),
        "an unrenderable card must answer \"too old\", not invent a status"
    );
}

/// Control: with no store wired (a bare `DiscordState`, as the unit tests and
/// any non-DB surface construct it) both paths keep working and never panic.
/// The store is additive, not a new requirement.
#[tokio::test]
async fn without_a_store_the_cards_stay_in_memory() {
    let state = DiscordState::new();
    state.upsert_tool_group(7009, group(2, false)).await;
    assert!(state.toggle_tool_group(7009).await.is_some());
    assert!(state.tool_group_snapshot(7009).await.is_some());

    state
        .store_long_answer(7010, ChannelId::new(1), vec!["only".to_string()])
        .await;
    assert!(state.long_answer_pages(7010).await.is_some());

    // The only thing that loses these is the restart itself, which is exactly
    // the gap the store closes.
    let fresh = DiscordState::new();
    assert!(fresh.tool_group_snapshot(7009).await.is_none());
    assert!(fresh.long_answer_pages(7010).await.is_none());
}

/// A card whose stored age reaches further back than this process's monotonic
/// clock must not take the press down with it. `Instant::now()` is time since
/// boot, so after a reboot a card written minutes earlier carries an age the
/// clock cannot place, and `now - age` panics on it. A reboot is the very
/// restart this store exists for, so the resume clamps instead of unwinding.
///
/// The age here is far larger than any uptime a host can report: the same
/// failure mode a reboot produces, reachable without rebooting.
#[tokio::test]
async fn an_age_past_the_clock_epoch_clamps_instead_of_panicking() {
    let snap = GroupSnapshot {
        entries: vec![EntrySnapshot {
            name: "tool0".to_string(),
            context: " (arg0)".to_string(),
            status: Some(true),
        }],
        notes: Vec::new(),
        expanded: true,
        started_secs_ago: u64::MAX / 2,
        activity_secs_ago: u64::MAX / 2,
        settled: None,
        live_ctx: None,
    };

    let group = GroupState::from_snapshot(snap).expect("the card is still renderable");
    let resumed = group.to_snapshot();

    assert!(
        resumed.started_secs_ago < 5,
        "an unplaceable age clamps to the epoch, got {}s",
        resumed.started_secs_ago
    );
    assert!(resumed.expanded, "the rest of the card survives the clamp");
    assert_eq!(resumed.entries.len(), 1);
}
