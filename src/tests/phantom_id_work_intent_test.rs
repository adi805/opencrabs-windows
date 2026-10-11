//! Work-intent narration that carries a work OBJECT, audit F1 of v0.5.10.
//!
//! The turn-end gate (`tool_loop.rs:8155`) and the four in-loop zero-tool gates
//! all consult `has_phantom_tool_intent_no_tools`. A zero-tool turn whose answer
//! narrated work reached the channel twice in one Discord session, and the
//! second one, "Gue jawab dari diff-nya langsung, bukan dari ingatan.", shipped
//! untouched because the pattern that could see it, `line_start_re`, is consumed
//! only by the two LOOP detectors (`count_intent_line_starts`,
//! `max_repeated_intent_line`) and never by that gate.
//!
//! The fix is deliberately narrow: first-person work verbs fire only when the
//! same line carries a work object, meaning a path, a file extension, a
//! backticked symbol, or a named command or tool. Precision assertions below
//! call `matches_work_intent_with_object` directly, because that is where the
//! object rule lives. A negative asserted through the whole gate could pass for
//! an unrelated reason and stop protecting the rule it claims to protect.

use crate::brain::agent::service::phantom::*;
use crate::brain::agent::service::phantom_lang::all_langs;

/// Every verb the new arm watches. A phrase dropped from `id.toml` fails here
/// rather than silently shrinking coverage.
const WATCHED_VERBS: [&str; 10] = [
    "gue tulis ",
    "gue susun ",
    "gue siapkan ",
    "gue rangkum ",
    "gue lacak ",
    "gue telusuri ",
    "gue jawab ",
    "gue paparkan ",
    "gue rakit ",
    "gue kumpulkan ",
];

/// Commentary about work already in context: the verb is there, the object is
/// not. Each of these must stay silent, or the arm trades a phantom for a
/// swallowed answer.
const COMMENTARY_WITHOUT_OBJECT: [&str; 4] = [
    "Gue rangkum hasilnya.",
    "Gue tulis ringkasannya.",
    "Gue susun rencananya.",
    "Gue kumpulkan catatannya.",
];

/// Opinion and agreement. A legitimate short reply replaced by the guard note
/// is worse than the gap staying open.
const OPINION_AND_AGREEMENT: [&str; 4] = [
    "Gue suka pendekatan itu.",
    "Gue setuju.",
    "Gue rasa itu bener.",
    "Gue kira aman.",
];

/// Someone else's action, mentioned in the third person. The phrase must OPEN
/// the line; an object elsewhere in the sentence does not turn a reported
/// action into this turn's narration.
const THIRD_PERSON_MENTIONS: [&str; 3] = [
    "Dia bilang gue tulis ringkasannya.",
    "Katanya gue tulis ringkasan ke AUDIT.md.",
    "Tadi dia bilang gue tulis ringkasan ke AUDIT.md.",
];

/// Ordinary command words that name no object. Excluded from `work_object_tools`
/// for the same reason the verbs are kept out of `intent_phrases`: the verb plus
/// a word like `bash` is still commentary, and flagging it would trade a phantom
/// for a swallowed answer.
const ORDINARY_COMMAND_WORDS: [&str; 3] = [
    "Gue rangkum opsi bash-nya.",
    "Gue tulis catatan grep-nya.",
    "Gue susun urutan bash dulu.",
];

#[test]
fn every_watched_verb_is_declared_exactly_once() {
    for verb in WATCHED_VERBS {
        let count = all_langs()
            .iter()
            .filter(|lang| lang.work_intent_phrases.iter().any(|p| p.as_str() == verb))
            .count();
        assert_eq!(count, 1, "{verb:?} must be declared exactly once");
    }
}

#[test]
fn the_watched_verbs_stay_out_of_the_bare_phrase_list() {
    // The design guarantee behind the whole arm. `intent_phrases` is matched
    // with a bare `contains` on a zero-tool turn, so a verb listed there would
    // also fire on "Gue rangkum hasilnya." and swap a legitimate sentence for
    // the guard note.
    for verb in WATCHED_VERBS {
        for lang in all_langs() {
            assert!(
                !lang.intent_phrases.iter().any(|p| p.as_str() == verb),
                "{verb:?} must stay out of intent_phrases"
            );
        }
    }
}

#[test]
fn a_work_intent_line_ending_on_a_file_extension_is_phantom() {
    assert!(has_phantom_tool_intent_no_tools(
        "Gue tulis ringkasan ke AUDIT.md."
    ));
}

#[test]
fn a_work_intent_line_naming_a_command_is_phantom() {
    assert!(has_phantom_tool_intent_no_tools(
        "Gue jalankan cargo test di src/tests/."
    ));
}

#[test]
fn a_work_intent_line_naming_a_backticked_symbol_is_phantom() {
    assert!(has_phantom_tool_intent_no_tools(
        "Gue lacak `TurnOutcome` di handler itu."
    ));
}

#[test]
fn commentary_without_a_work_object_is_not_phantom() {
    for text in COMMENTARY_WITHOUT_OBJECT {
        assert!(!matches_work_intent_with_object(text), "{text:?}");
    }
}

#[test]
fn ordinary_command_words_are_not_a_work_object() {
    // The tool list is kept to unmistakable names on purpose. `bash` and `grep`
    // are ordinary words in dev chat, so a line that merely mentions one is
    // commentary, and the verb plus that word must stay silent.
    for text in ORDINARY_COMMAND_WORDS {
        assert!(!matches_work_intent_with_object(text), "{text:?}");
    }
}

#[test]
fn opinion_and_agreement_are_not_phantom() {
    // The failure mode this gate exists to prevent: a legitimate short reply
    // replaced by the guard note is worse than the gap being closed.
    for text in OPINION_AND_AGREEMENT {
        assert!(!matches_work_intent_with_object(text), "{text:?}");
        assert!(!has_phantom_tool_intent_no_tools(text), "{text:?}");
    }
}

#[test]
fn a_third_person_mention_is_not_phantom() {
    for text in THIRD_PERSON_MENTIONS {
        assert!(!matches_work_intent_with_object(text), "{text:?}");
    }
}

#[test]
fn the_incident_sentence_stays_uncaught_by_design() {
    // Audit F1's trigger sentence carries no work object, so it sits outside
    // this arm on purpose: catching it would mean catching "Gue setuju." too.
    // If this ever flips, the object rule was widened, which is a decision
    // rather than a drive-by.
    let incident = "Gue jawab dari diff-nya langsung, bukan dari ingatan.";
    assert!(!matches_work_intent_with_object(incident));
}

#[test]
fn a_structured_answer_quoting_the_phrase_is_not_phantom() {
    // The #463 exemption: a table or code block inevitably quotes phrases we
    // watch for, and a structured report is never a phantom.
    let table = "| Gue tulis ringkasan ke AUDIT.md. | catatan |\n|---|---|\n| satu | dua |";
    assert!(!has_phantom_tool_intent_no_tools(table));

    let fenced = "Ringkasannya:\n\n```\nGue tulis ringkasan ke AUDIT.md.\n```";
    assert!(!has_phantom_tool_intent_no_tools(fenced));
}
