//! #1984 (typing-tick half): a child parked at `AwaitingInput` belongs to a
//! human, not the machine, so the handover typing indicator must not keep
//! ticking because of it. The #1985 extraction first counted
//! `working + awaiting`, which pins the Telegram/Discord tick loop open on a
//! follow-up that may never arrive, exactly the stuck clock #1984 reports.
//! These tests pin the predicate: awaiting-only returns with zero pings, a
//! mid-round child keeps the pings coming, and an unwired surface sends
//! nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use uuid::Uuid;

use crate::brain::tools::subagent::manager::{SubAgent, SubAgentManager, SubAgentState};
use crate::channels::typing_tick::tick_while_detached;

fn child(parent: Uuid, awaiting: bool) -> SubAgent {
    SubAgent {
        state: if awaiting {
            SubAgentState::AwaitingInput
        } else {
            SubAgentState::Running
        },
        ..SubAgent::new(
            Uuid::new_v4().to_string(),
            "test child",
            Uuid::new_v4(),
            parent,
        )
    }
}

/// Count pings for a 150 ms budget at a 10 ms tick. A still-looping case
/// gets cancelled by the timeout; every settled case returned on its own.
async fn run_tick(agents: Option<Arc<SubAgentManager>>, session: Uuid) -> usize {
    let pings = Arc::new(AtomicUsize::new(0));
    let counted = pings.clone();
    let tick = tick_while_detached(
        None,
        agents,
        session,
        Duration::from_millis(10),
        move || {
            let pings = counted.clone();
            async move {
                pings.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    let _ = tokio::time::timeout(Duration::from_millis(150), tick).await;
    pings.load(Ordering::SeqCst)
}

#[tokio::test]
async fn awaiting_children_do_not_hold_the_typing_tick() {
    // #1984 acceptance: typing stops while every child is parked on a human.
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, true));
    mgr.insert(child(session, true));
    let pings = run_tick(Some(mgr), session).await;
    assert_eq!(pings, 0, "awaiting-only session must not ping at all");
}

#[tokio::test]
async fn working_children_hold_the_typing_tick() {
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, false));
    let pings = run_tick(Some(mgr), session).await;
    assert!(pings > 0, "a mid-round child must keep the tick alive");
}

#[tokio::test]
async fn unwired_surface_sends_nothing() {
    let pings = run_tick(None, Uuid::new_v4()).await;
    assert_eq!(pings, 0);
}
