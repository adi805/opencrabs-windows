//! Seam tests for the restart-resume delivery rewrite (#1950, #1951, #1952,
//! #1953).
//!
//! No network and no tokio runtime: the presence guard rides a real
//! `TuiEvent` bus but unbounded sends are synchronous, the notice text and
//! chunking are pure functions, and the ledger gate is a pure predicate.
//! What these pin is exactly what the drop tests of 2026-10-05 could not:
//! that a resumed turn announces itself, that only answers that reached a
//! surface count as delivered, and that a 5000-char Discord answer ships
//! instead of dying.

use uuid::Uuid;

use crate::cli::resume_delivery::{
    ResumePresence, ResumeSendOutcome, chunks_for, resume_notice_text,
};
use crate::tui::events::TuiEvent;

#[test]
fn restart_resume_receipts_painted_and_cleared_for_remote_rows() {
    // #1951: the guard must emit the SAME events the channel ingress emits
    // for live turns, on the real event type, or the TUI stays blind.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();
    let id = Uuid::new_v4();
    let guard = ResumePresence::start(&tx, id, "discord").expect("remote rows carry presence");
    let started = rx.try_recv().expect("started event on creation");
    assert!(
        matches!(started, TuiEvent::ChannelProcessingStarted(g) if g == id),
        "expected ChannelProcessingStarted({id}), got {started:?}"
    );
    drop(guard);
    let finished = rx.try_recv().expect("finished event on Drop");
    assert!(
        matches!(finished, TuiEvent::ChannelProcessingFinished(g) if g == id),
        "expected ChannelProcessingFinished({id}), got {finished:?}"
    );
    assert!(rx.try_recv().is_err(), "exactly one started/finished pair");
}

#[test]
fn restart_resume_receipts_absent_for_tui_rows() {
    // TUI rows already carry the PendingResumed card; a processing badge
    // would additionally claim the local turn is a remote one.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();
    let guard = ResumePresence::start(&tx, Uuid::new_v4(), "tui");
    assert!(guard.is_none(), "tui rows must not double-presence");
    assert!(rx.try_recv().is_err(), "no events for tui rows");
}

#[test]
fn restart_resume_receipts_notice_names_the_session() {
    // #1950: the channel-side announcement the drop test never got.
    let id = Uuid::new_v4();
    let text = resume_notice_text(id);
    assert!(
        text.starts_with("🔁"),
        "notice must be recognizable: {text}"
    );
    let short = id.simple().to_string()[..8].to_string();
    assert!(
        text.contains(&short),
        "notice must carry the session id: {text}"
    );
}

#[test]
fn restart_resume_receipts_only_reached_surfaces_count() {
    // #1952: the metric that lied. Everything that is not Sent/SurfacedTui
    // must stay out of the delivered ledger, or the boot summary hides drops
    // behind a green number again.
    assert!(ResumeSendOutcome::Sent.counts_as_delivered());
    assert!(ResumeSendOutcome::SurfacedTui.counts_as_delivered());
    for gone in [
        ResumeSendOutcome::SendFailed,
        ResumeSendOutcome::TransportGone,
        ResumeSendOutcome::NoAddress,
        ResumeSendOutcome::BadAddress,
        ResumeSendOutcome::Unsupported,
    ] {
        assert!(!gone.counts_as_delivered(), "{gone:?} reached no surface");
    }
}

#[cfg(feature = "discord")]
#[test]
fn restart_resume_receipts_long_discord_answers_split() {
    // #1953: the whole point of chunking is that the user gets the answer.
    // A cap violation on one `say` must not be able to eat 5000 chars.
    let text = "a".repeat(5000);
    let chunks = chunks_for("discord", &text);
    assert!(chunks.len() > 1, "5000 chars must not ride one message");
    for (i, chunk) in chunks.iter().enumerate() {
        assert!(
            chunk.len() <= 2000,
            "chunk {i} is {} bytes, over the cap",
            chunk.len()
        );
    }
    assert_eq!(chunks.concat(), text, "the seams must not lose a byte");
}

#[cfg(feature = "discord")]
#[test]
fn restart_resume_receipts_short_answers_stay_whole() {
    let chunks = chunks_for("discord", "hello");
    assert_eq!(chunks, vec!["hello".to_string()]);
}

#[test]
fn restart_resume_receipts_other_channels_keep_single_path() {
    // The proven cap disease is Discord's; whatsapp/slack ride the single
    // send until their limits have an occurrence to justify chunking.
    let text = "x".repeat(5000);
    assert_eq!(chunks_for("whatsapp", &text).len(), 1);
    assert_eq!(chunks_for("slack", &text).len(), 1);
}
