//! Inline mode's gate and result set (#99, #109).
//!
//! An inline query can be typed into any chat on Telegram by any user, so the
//! result set is the widest surface the channel has. These tests pin the two
//! properties that keep it narrow: a non-owner never receives the command set,
//! and the owner's set is a bounded table that cannot grow into generated text.

use crate::channels::commands::format_help;
use crate::channels::telegram::inline::{
    INLINE_COMMANDS, OWNER_ONLY_NOTICE, build_answer_body, build_results,
};
use serde_json::Value;

fn results_for(is_owner: bool) -> Vec<Value> {
    build_results(is_owner)
        .as_array()
        .expect("build_results must return an array of results")
        .clone()
}

/// Everything a result would insert or display, so a leak can be searched for
/// across the whole payload rather than in one field a test remembered to check.
fn haystack(results: &[Value]) -> String {
    serde_json::to_string(results).expect("results must serialize")
}

#[test]
fn owner_gets_one_result_per_command() {
    let results = results_for(true);
    assert_eq!(
        results.len(),
        INLINE_COMMANDS.len(),
        "every inline command needs exactly one result"
    );
    for (i, (cmd, description)) in INLINE_COMMANDS.iter().enumerate() {
        assert_eq!(results[i]["title"], *cmd);
        assert_eq!(results[i]["description"], *description);
    }
}

#[test]
fn every_result_is_a_paste_ready_article() {
    for result in results_for(true) {
        assert_eq!(result["type"], "article", "got {result}");
        assert_eq!(
            result["input_message_content"]["message_text"], result["title"],
            "the inserted text must be the command itself, not a sentence about it: {result}"
        );
    }
}

#[test]
fn result_ids_are_unique() {
    // Telegram rejects the whole answer on a duplicate id, and a duplicate
    // would also make one command unreachable in the picker.
    let mut seen = std::collections::HashSet::new();
    for result in results_for(true) {
        let id = result["id"].as_str().expect("every result needs an id");
        assert!(seen.insert(id.to_string()), "{id} is used twice");
    }
}

#[test]
fn non_owner_gets_only_the_notice() {
    let results = results_for(false);
    assert_eq!(results.len(), 1, "a non-owner must get exactly one result");
    assert_eq!(
        results[0]["input_message_content"]["message_text"],
        OWNER_ONLY_NOTICE
    );
    // The load-bearing assertion: not "the notice is present" but "no command
    // content is", checked over the serialized payload so a command smuggled
    // into any other field is still caught.
    let payload = haystack(&results);
    for (cmd, _) in INLINE_COMMANDS {
        assert!(
            !payload.contains(cmd),
            "{cmd} leaked into a non-owner's inline results: {payload}"
        );
    }
}

#[test]
fn owner_result_set_is_bounded() {
    // The bound is the point of the module: a set that can grow with generated
    // content is a second agent loop reachable from any chat on Telegram.
    assert!(
        INLINE_COMMANDS.len() <= 12,
        "the inline set is meant to stay a short table, not a menu: {} entries",
        INLINE_COMMANDS.len()
    );
}

#[test]
fn inline_commands_cannot_drift_from_the_help_text() {
    // The table is a curated subset rather than a copy of the help list, so the
    // pin that matters is that every entry is still a real command. A rename in
    // commands.rs would otherwise leave an inline result pasting a dead command.
    let help = format_help();
    for (cmd, _) in INLINE_COMMANDS {
        assert!(
            help.contains(cmd),
            "{cmd} is offered inline but is not in the help text, so it is either \
             renamed or not a command"
        );
    }
}

#[test]
fn answer_body_is_personal_and_uncached() {
    // Telegram caches inline answers per query text unless told otherwise, so a
    // cached owner answer served to the next user is the exact leak this module
    // guards against.
    let body = build_answer_body("q-1", &build_results(true));
    assert_eq!(body["inline_query_id"], "q-1");
    assert_eq!(body["is_personal"], true);
    assert_eq!(body["cache_time"], 0);
}

#[test]
fn answer_body_carries_the_results_it_was_given() {
    // Negative control for the body builder: a body that dropped the results
    // would still satisfy every field assertion above.
    let owner = build_answer_body("q-2", &build_results(true));
    let stranger = build_answer_body("q-2", &build_results(false));
    assert_eq!(
        owner["results"].as_array().map(Vec::len),
        Some(INLINE_COMMANDS.len())
    );
    assert_eq!(stranger["results"].as_array().map(Vec::len), Some(1));
    assert_ne!(owner["results"], stranger["results"]);
}
