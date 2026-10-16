//! #2028: a Discord turn that halts on `suggest_options` posts the answer
//! that came right before the options tool, before the buttons. The rules
//! that pick that answer are pure, so they are pinned here.

use crate::channels::discord::options_host::{OptionsHost, note_belongs_to};

#[test]
fn text_directly_before_options_is_the_host() {
    let mut host = OptionsHost::default();
    host.on_text("Here is the answer.");
    host.on_tool_started("suggest_options");
    assert_eq!(host.take().as_deref(), Some("Here is the answer."));
}

#[test]
fn text_before_another_tool_is_not_the_host() {
    // The text belonged to an earlier step, not to the options surface.
    let mut host = OptionsHost::default();
    host.on_text("Checking the file.");
    host.on_tool_started("bash");
    host.on_tool_started("suggest_options");
    assert_eq!(host.take(), None);
}

#[test]
fn options_without_text_has_no_host() {
    let mut host = OptionsHost::default();
    host.on_tool_started("suggest_options");
    assert_eq!(host.take(), None);
}

#[test]
fn host_is_handed_over_once() {
    let mut host = OptionsHost::default();
    host.on_text("Answer.");
    host.on_tool_started("suggest_options");
    assert!(host.take().is_some());
    assert_eq!(host.take(), None);
}

#[test]
fn texts_in_one_iteration_join_in_order() {
    let mut host = OptionsHost::default();
    host.on_text("First part.");
    host.on_text("Second part.");
    host.on_tool_started("suggest_options");
    assert_eq!(host.take().as_deref(), Some("First part.\n\nSecond part."));
}

#[test]
fn text_after_an_earlier_tool_starts_a_new_host() {
    let mut host = OptionsHost::default();
    host.on_text("Old narration.");
    host.on_tool_started("bash");
    host.on_text("The real answer.");
    host.on_tool_started("suggest_options");
    assert_eq!(host.take().as_deref(), Some("The real answer."));
}

#[test]
fn clipped_note_matches_its_host_first_line() {
    let line = "a".repeat(200);
    let clipped = format!("{}…", "a".repeat(160));
    assert!(note_belongs_to(&clipped, &line));
}

#[test]
fn full_short_note_matches_case_insensitively() {
    assert!(note_belongs_to("Hello there", "hello there, friend"));
}

#[test]
fn unrelated_note_does_not_match() {
    assert!(!note_belongs_to(
        "Checking the file.",
        "here is the answer."
    ));
}

#[test]
fn empty_note_never_matches() {
    // An empty core would prefix-match every host and drop an unrelated note.
    assert!(!note_belongs_to("…", "any host text"));
    assert!(!note_belongs_to("", "any host text"));
}
