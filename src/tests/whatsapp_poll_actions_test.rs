//! Poll actions on the WhatsApp send tool: quizzes, casting a vote, opening a
//! raw vote, and reading the tally back.
//!
//! The arms need a live socket, so the wire calls cannot run here. What is
//! checkable without a connection is the part that decides what the model can
//! reach and which call each action makes: the dispatch table, the argument
//! rules the arms refuse on, and where the poll's message secret comes from.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The receiver is part of the pin: a quiz answered by the plain-poll path, or
/// a tally that re-decrypts instead of reading what was recorded, would still
/// compile.
const ACTIONS: &[(&str, &str)] = &[
    ("create_quiz", "polls().create_quiz("),
    ("vote", "polls().vote("),
    ("decrypt_vote", "polls().decrypt_vote("),
    ("aggregate_votes", "votes.for_poll("),
];

/// The text with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The body of the match arm that handles `action`: from its pattern to the
/// start of the next arm.
///
/// Pins are scoped to the arm on purpose. A file-wide `contains` would be
/// satisfied by a call living anywhere in a three-thousand-line file, so an
/// action whose arm was deleted while its helper stayed in use elsewhere would
/// still pass.
fn arm_body(action: &str) -> &'static str {
    let needle = format!("\"{action}\"");
    let mut from = 0;
    loop {
        let at = SOURCE[from..]
            .find(&needle)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("'{action}' does not appear in the source"));
        let after = &SOURCE[at + needle.len()..];
        // An arm pattern reaches `=> {` on its own line, right after the name:
        // a grouped arm such as `"a" | "b" => {` included. A hit further away,
        // or one that crosses a newline, is the schema enum or an error string.
        if let Some(rel) = after.find("=> {")
            && rel < 80
            && !after[..rel].contains('\n')
        {
            let rest = &after[rel..];
            let ends = [
                rest.find("\n            \""),
                rest.find("\n            unknown"),
            ];
            let end = ends.into_iter().flatten().min().unwrap_or(rest.len());
            return &rest[..end];
        }
        from = at + needle.len();
    }
}

#[test]
fn every_poll_action_has_a_match_arm_and_a_schema_entry() {
    let compact = squash(SOURCE);
    for (action, _) in ACTIONS {
        let occurrences = compact.matches(&format!("\"{action}\"")).count();
        assert!(
            occurrences >= 2,
            "'{action}' appears {occurrences} time(s): it needs both a schema enum entry \
             (so the model can discover it) and a match arm"
        );
    }
}

#[test]
fn the_poll_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// Option 0 is a real answer, so a missing `quiz_correct_index` must not read
/// as "the first option". The arm refuses instead of defaulting, which is the
/// difference between a quiz that marks the right answer and one that marks
/// whichever option happened to be written first.
#[test]
fn a_quiz_cannot_be_created_without_a_correct_answer() {
    let body = squash(arm_body("create_quiz"));
    assert!(
        body.contains("letSome(correct_index)=input.get(\"quiz_correct_index\")"),
        "create_quiz must require 'quiz_correct_index' rather than defaulting it to 0"
    );
    assert!(
        arm_body("create_quiz").contains("create_quiz needs 'quiz_correct_index'"),
        "create_quiz must say which parameter is missing"
    );
}

/// An index past the end of the option list would produce a quiz whose correct
/// answer points at nothing, so it is refused before the send.
#[test]
fn a_correct_answer_outside_the_option_list_is_refused() {
    assert!(
        squash(arm_body("create_quiz")).contains("ifcorrect_index>=opts.len()"),
        "create_quiz must bound the correct index by the option count"
    );
}

/// A poll and a quiz are the same shape on the wire, so they share the option
/// rule. Two copies of the 2..12 check would drift, and a quiz with one option
/// has nothing to choose between.
#[test]
fn send_poll_and_create_quiz_share_one_option_rule() {
    for action in ["send_poll", "create_quiz"] {
        assert!(
            squash(arm_body(action)).contains("poll_options(&input)"),
            "'{action}' must parse its options through the shared helper"
        );
    }
    let helper = squash(SOURCE);
    assert!(
        helper.contains("fnpoll_options(input:&Value)"),
        "the shared option parser is missing"
    );
}

/// A poll, a quiz and a vote all put a message on the wire and are charged to
/// the send budget. Opening a vote and reading a tally are pure reads, so
/// charging them would spend budget on an answer that sends nothing.
#[test]
fn a_vote_is_charged_to_the_send_budget_and_a_tally_is_not() {
    for action in ["create_quiz", "vote"] {
        assert!(
            squash(arm_body(action)).contains("gate_send!(self,jid_str,"),
            "'{action}' sends a message and must be charged to the send budget"
        );
    }
    for action in ["decrypt_vote", "aggregate_votes"] {
        assert!(
            !squash(arm_body(action)).contains("gate_send!"),
            "'{action}' sends nothing and must not be charged to the send budget"
        );
    }
}

/// The secret a poll's votes are encrypted against is minted at creation and
/// persisted for every poll this account authored, so the stored one is the
/// common case and the caller's is the fallback. Asking for the fallback first
/// would make every ordinary vote require a parameter nobody has.
#[test]
fn the_stored_secret_is_preferred_over_the_caller_s() {
    let compact = squash(SOURCE);
    assert!(
        compact.contains("ifinput.get(\"poll_secret\").and_then(|v|v.as_str()).is_some(){returndecode_b64(input,\"poll_secret\");}"),
        "resolve_poll_secret must only decode 'poll_secret' when it was actually given"
    );
    assert!(
        compact.contains("get_msg_secret(chat,creator,poll_id)"),
        "resolve_poll_secret must fall back to the stored message secret"
    );
    assert!(
        compact.contains("withoutitavotecannotbeencryptedordecrypted"),
        "a missing secret must be reported, not passed through as an empty key"
    );
}

/// Both crypto paths key on the poll's author, and the only author whose secret
/// this account holds is itself, so an omitted `poll_creator_jid` means us.
/// Falling back to whoever is talking would key the vote on the wrong account
/// and produce a ciphertext nobody can open.
#[test]
fn the_creator_defaults_to_this_account() {
    assert!(
        squash(SOURCE).contains("None=>client.pn().ok_or_else(||"),
        "an omitted poll creator must default to this account's own JID"
    );
}

/// The tally is built by the inbound decoder, which resolves each arriving vote
/// to labels as it lands. The lib's own aggregate needs every voter's raw
/// ciphertext, and those are not retained, so re-decrypting here is impossible
/// rather than merely slower: this action must read what was recorded.
#[test]
fn aggregate_votes_reads_the_recorded_tally_instead_of_re_decrypting() {
    let body = squash(arm_body("aggregate_votes"));
    assert!(
        body.contains("self.whatsapp_state.votes.for_poll(&poll_id).await"),
        "aggregate_votes must read the recorded tally"
    );
    assert!(
        !body.contains("polls().aggregate_votes("),
        "aggregate_votes must not call the lib's crypto aggregate, which needs ciphertexts \
         this path never holds"
    );
    assert!(
        arm_body("aggregate_votes").contains("No votes recorded for poll"),
        "an empty tally must be reported as unrecorded rather than as zero votes"
    );
}

/// A vote lands in a chat, which may be a group, so it resolves through
/// `resolve_chat_jid`; the poll's author and the voter are people and must not
/// accept a group id.
#[test]
fn a_vote_names_a_chat_while_the_creator_is_a_person() {
    for action in ["vote", "decrypt_vote"] {
        assert!(
            arm_body(action).contains("resolve_chat_jid("),
            "'{action}' must resolve its chat through resolve_chat_jid"
        );
    }
    assert!(
        squash(arm_body("decrypt_vote"))
            .contains("parse_contact_jid(pget!(get_str(&input,\"voter_jid\")),\"voter_jid\")"),
        "decrypt_vote must read the voter as a person, not as a chat"
    );
}
