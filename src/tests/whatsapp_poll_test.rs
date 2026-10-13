//! Inbound poll vote resolution (#1482).

use crate::channels::whatsapp::poll::{
    POLL_CAPACITY, PollOptions, VOTE_TALLY_CAPACITY, VoteTally, describe_vote, resolve_options,
};
use wacore::poll::compute_option_hash;

fn hash(option: &str) -> Vec<u8> {
    compute_option_hash(option).to_vec()
}

fn options() -> Vec<String> {
    vec!["Rust".to_string(), "Zig".to_string(), "C".to_string()]
}

#[test]
fn a_single_vote_resolves_to_its_label() {
    assert_eq!(resolve_options(&[hash("Zig")], &options()), vec!["Zig"]);
}

#[test]
fn a_multi_vote_resolves_every_label() {
    let selected = vec![hash("C"), hash("Rust")];
    assert_eq!(
        resolve_options(&selected, &options()),
        vec!["Rust", "C"],
        "order follows the poll, not the vote, so two voters read identically"
    );
}

#[test]
fn an_unknown_hash_is_dropped_rather_than_shown_as_hex() {
    let selected = vec![hash("Rust"), hash("something we never offered")];
    assert_eq!(resolve_options(&selected, &options()), vec!["Rust"]);
}

#[test]
fn a_vote_for_nothing_resolves_to_nothing() {
    assert!(resolve_options(&[], &options()).is_empty());
}

#[test]
fn a_poll_with_no_options_resolves_nothing() {
    assert!(resolve_options(&[hash("Rust")], &[]).is_empty());
}

#[test]
fn option_hashing_is_what_the_lib_does() {
    // If this ever stops matching, votes silently resolve to nothing rather
    // than failing loudly, so it is worth pinning.
    assert_eq!(compute_option_hash("Rust").len(), 32);
    assert_ne!(compute_option_hash("Rust"), compute_option_hash("rust"));
}

#[test]
fn a_vote_renders_with_the_voter_and_the_choices() {
    assert_eq!(
        describe_vote("15551234", &["Rust".to_string(), "C".to_string()]).as_deref(),
        Some("[poll vote] 15551234 chose: Rust, C")
    );
}

#[test]
fn a_cleared_vote_renders_nothing() {
    // The voter deselected everything. Reporting "chose:" with an empty list
    // would read as a vote that was never cast.
    assert_eq!(describe_vote("15551234", &[]), None);
}

#[tokio::test]
async fn poll_options_round_trip() {
    let polls = PollOptions::default();
    polls.remember("3EB0POLL", options()).await;
    assert_eq!(polls.get("3EB0POLL").await, Some(options()));
}

#[tokio::test]
async fn an_unknown_poll_has_no_options() {
    let polls = PollOptions::default();
    assert!(polls.get("3EB0MISSING").await.is_none());
}

#[tokio::test]
async fn an_empty_id_or_option_list_is_not_stored() {
    let polls = PollOptions::default();
    polls.remember("", options()).await;
    polls.remember("3EB0EMPTY", Vec::new()).await;
    assert_eq!(polls.len().await, 0);
}

#[tokio::test]
async fn the_oldest_poll_is_evicted_once_the_window_is_full() {
    let polls = PollOptions::default();
    for i in 0..=POLL_CAPACITY {
        polls.remember(format!("poll-{i}"), options()).await;
    }
    assert_eq!(polls.len().await, POLL_CAPACITY);
    assert!(polls.get("poll-0").await.is_none());
    assert!(polls.get(&format!("poll-{POLL_CAPACITY}")).await.is_some());
}

#[tokio::test]
async fn re_remembering_a_poll_replaces_its_options() {
    let polls = PollOptions::default();
    polls.remember("p", vec!["old".to_string()]).await;
    polls.remember("p", vec!["new".to_string()]).await;
    assert_eq!(polls.get("p").await, Some(vec!["new".to_string()]));
    assert_eq!(polls.len().await, 1);
}

/// A voter who changes their answer replaces it, matching WhatsApp itself. Two
/// entries for one voter would make the tally read as two people.
#[tokio::test]
async fn a_voter_changing_their_answer_replaces_it() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Zig".to_string()])
        .await;
    let tally = votes.for_poll("p").await;
    assert_eq!(tally.len(), 1);
    assert_eq!(tally[0].1, vec!["Zig".to_string()]);
}

/// A voter is keyed on their resolved JID, never their display name: two
/// people who both call themselves "Adi" are two voters.
#[tokio::test]
async fn two_voters_sharing_a_display_name_stay_two_voters() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Adi", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "222@s.whatsapp.net", "Adi", vec!["Zig".to_string()])
        .await;
    let tally = votes.for_poll("p").await;
    assert_eq!(
        tally.len(),
        2,
        "a shared display name must not collapse two voters into one"
    );
}

/// The other half of the same bug: one voter who renames themselves is still
/// one voter, and the newest name is the one rendered.
#[tokio::test]
async fn one_voter_renaming_stays_one_voter() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Adi", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Adi Jr", vec!["Zig".to_string()])
        .await;
    let tally = votes.for_poll("p").await;
    assert_eq!(tally.len(), 1, "a rename must not create a second voter");
    assert_eq!(tally[0].0, "Adi Jr");
    assert_eq!(tally[0].1, vec!["Zig".to_string()]);
}

/// A withdrawal REMOVES the voter's entry. Leaving it behind would keep
/// counting a vote the voter has taken back.
#[tokio::test]
async fn a_withdrawn_vote_is_removed() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Ana", Vec::new())
        .await;
    assert!(
        votes.for_poll("p").await.is_empty(),
        "clearing an answer must stop it being counted"
    );
}

/// A withdrawal touches only the voter who withdrew.
#[tokio::test]
async fn a_withdrawal_leaves_other_voters_alone() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "222@s.whatsapp.net", "Budi", vec!["Zig".to_string()])
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Ana", Vec::new())
        .await;
    let tally = votes.for_poll("p").await;
    assert_eq!(tally.len(), 1);
    assert_eq!(tally[0].0, "Budi");
}

/// Withdrawing without ever voting is a no-op, not an entry with no answer.
#[tokio::test]
async fn a_withdrawal_with_no_prior_vote_records_nothing() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Ana", Vec::new())
        .await;
    assert!(votes.for_poll("p").await.is_empty());
}

/// A poll emptied by withdrawal leaves the tracked set, and answering it again
/// afterwards must not file the poll twice.
#[tokio::test]
async fn a_poll_emptied_by_withdrawal_can_be_answered_again() {
    let votes = VoteTally::default();
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Rust".to_string()])
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Ana", Vec::new())
        .await;
    votes
        .record("p", "111@s.whatsapp.net", "Ana", vec!["Zig".to_string()])
        .await;
    let tally = votes.for_poll("p").await;
    assert_eq!(tally.len(), 1);
    assert_eq!(tally[0].1, vec!["Zig".to_string()]);
}

/// The tally is bounded like the option window: a long-lived account cannot
/// grow it without limit, and a poll dropped from the window reads as
/// unrecorded rather than as a wrong count.
#[tokio::test]
async fn the_tally_keeps_only_the_newest_polls() {
    let votes = VoteTally::default();
    for i in 0..=VOTE_TALLY_CAPACITY {
        votes
            .record(
                &format!("poll-{i}"),
                "111@s.whatsapp.net",
                "Ana",
                vec!["Rust".to_string()],
            )
            .await;
    }
    assert!(votes.for_poll("poll-0").await.is_empty());
    assert_eq!(
        votes
            .for_poll(&format!("poll-{VOTE_TALLY_CAPACITY}"))
            .await
            .len(),
        1
    );
}

/// Votes are filed per poll, so two polls running at once do not share a count.
#[tokio::test]
async fn the_tally_is_per_poll() {
    let votes = VoteTally::default();
    votes
        .record("a", "111@s.whatsapp.net", "Ana", vec!["Rust".to_string()])
        .await;
    votes
        .record("b", "222@s.whatsapp.net", "Budi", vec!["Zig".to_string()])
        .await;
    assert_eq!(votes.for_poll("a").await.len(), 1);
    assert_eq!(votes.for_poll("b").await[0].0, "Budi");
}
