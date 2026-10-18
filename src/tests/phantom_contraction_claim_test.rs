//! #2110: an English contraction before a listed action verb was read as a
//! completed-action claim, so a zero-tool turn's real answer got replaced by
//! the guard note.
//!
//! Reported turn (guest session `4a7924de`, 2026-10-18): the answer
//! "Loud and clear. ... Well, everything I'm tagged in, anyway." was swapped
//! for "I described actions but did not actually execute any tool this turn".
//! `tagged` is in `action_verbs`, and the auxiliary guard that exempts passive
//! description knew `is/are/was/...` but no contractions, so `i'm` before the
//! verb never exempted the sentence.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::phantom::has_past_tense_action_claim;

/// The subset of `action_verbs` these cases exercise, spelled out so the test
/// pins the guard rather than the shipped word list.
fn verbs() -> Vec<String> {
    vec![
        "tagged".to_string(),
        "loaded".to_string(),
        "pushed".to_string(),
        "committed".to_string(),
    ]
}

#[test]
fn a_contraction_before_a_listed_verb_is_not_a_claim() {
    for text in [
        // The reported sentence, minus the surrounding chatter.
        "well, everything i'm tagged in, anyway",
        "it's loaded",
        "they're committed to it",
        "you're tagged in the thread",
        "she's pushed it upstream",
    ] {
        assert!(
            !has_past_tense_action_claim(text, &verbs()),
            "a copula contraction describes state, not a completion claim: {text:?}"
        );
    }
}

#[test]
fn an_active_claim_without_an_auxiliary_still_fires() {
    for text in [
        "tagged in the thread",
        "loaded",
        "pushed the branch",
        "committed and pushed",
    ] {
        assert!(
            has_past_tense_action_claim(text, &verbs()),
            "a bare past-tense verb is still a completion claim: {text:?}"
        );
    }
}

#[test]
fn possessive_have_is_not_a_copula_and_still_fires() {
    // "I've pushed the branch" is an ACTIVE claim. The guard must not absorb
    // it just because it is also a contraction.
    assert!(
        has_past_tense_action_claim("i've pushed the branch", &verbs()),
        "'i have' sits in front of an active claim and must keep firing"
    );
}
