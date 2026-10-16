//! Admission for the Slack block actions that touch the SHARED session
//! (F-CH-02, SEC-05).
//!
//! `on_interaction` runs on a message in a channel, so anyone who can see that
//! message can press the buttons on it. Two of the arms re-check identity: the
//! `session:` arm (a non-owner gets a private session instead of the shared
//! one) and the approval buttons (`approve:`/`always:`/`yolo:`/`deny:`, OC-01).
//! The `provider:` and `model:` arms shipped with no gate at all, and both read
//! `state.shared_session` and call `swap_provider_for_session` on it. A caller
//! whose messages the message path refuses (deny-by-default, OC-02) could
//! therefore still re-point the provider for the whole room.
//!
//! The gate has to be a real admission decision AND it has to run before the
//! shared session is read: authorizing afterwards still lets a refused tap
//! reach `swap_provider_for_session`. These tests pin the decision table on
//! `SlackConfig::is_owner` and, in source form, the ordering.
//!
//! The source-shape half is legitimate under `CONTRIBUTING.md` ("Where Tests
//! Live") because the ordering is unreachable from a unit test: `on_interaction`
//! needs a live `SlackHyperClient`, and a gate that runs one statement too late
//! still compiles and still passes every behavioural test.

use crate::config::types::SlackConfig;
use crate::tests::source_shape::read_in;

const HANDLER: &str = "src/channels/slack/handler.rs";

// ── the decision table ──────────────────────────────────────────────

#[test]
fn an_unconfigured_workspace_admits_nobody() {
    let sc = SlackConfig::default();
    assert!(
        sc.allowed_users.is_empty() && sc.bot_owner.is_empty(),
        "fixture sanity: the default config is the unconfigured case"
    );
    assert!(
        !sc.is_owner("U0ANYONE"),
        "F-CH-02: an unconfigured workspace is deny-by-default, exactly as the \
         message path already treated it (OC-02)"
    );
}

#[test]
fn the_configured_owner_may_act_on_the_shared_session() {
    let sc = SlackConfig {
        bot_owner: vec!["UOWNER".to_string()],
        ..Default::default()
    };
    assert!(sc.is_owner("UOWNER"));
}

#[test]
fn an_allowlisted_guest_may_not_act_on_the_shared_session() {
    // The divergence this finding is about: the message path admits an
    // allowlisted non-owner so they can talk to the bot, while the shared
    // session stays the owner's. The gate deliberately uses the narrower rule,
    // the same one the neighbouring `session:` arm applies.
    let sc = SlackConfig {
        allowed_users: vec!["UOWNER".to_string(), "UGUEST".to_string()],
        ..Default::default()
    };
    assert!(
        sc.is_owner("UOWNER"),
        "without an explicit bot_owner the first allow-list entry is the owner"
    );
    assert!(
        !sc.is_owner("UGUEST"),
        "F-CH-02: an allowlisted guest must not re-point the provider for the room"
    );
}

#[test]
fn a_caller_absent_from_the_config_is_refused() {
    let sc = SlackConfig {
        bot_owner: vec!["UOWNER".to_string()],
        allowed_users: vec!["UGUEST".to_string()],
        ..Default::default()
    };
    assert!(
        !sc.is_owner("USTRANGER"),
        "a caller in neither list is not the owner"
    );
}

// ── the wiring ──────────────────────────────────────────────────────

fn line_of(lines: &[&str], needle: &str, from: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
}

/// The index of the last line at or before `before` containing `needle`.
fn line_before(lines: &[&str], needle: &str, before: usize) -> Option<usize> {
    lines[..before].iter().rposition(|l| l.contains(needle))
}

/// `on_interaction`'s body, as lines. Bounded by the next top-level item so a
/// later function's shared-session writes cannot be attributed to this one.
fn on_interaction_body(src: &str) -> Vec<&str> {
    let lines: Vec<&str> = src.lines().collect();
    let start = line_of(&lines, "pub async fn on_interaction(", 0)
        .unwrap_or_else(|| panic!("{HANDLER}: on_interaction is gone; re-check this test"));
    let end = line_of(&lines, "/// Global handler state", start)
        .unwrap_or_else(|| panic!("{HANDLER}: cannot find the end of on_interaction"));
    lines[start..end].to_vec()
}

/// A gate that runs after the shared session is read is not a gate.
fn assert_arm_gates_before_reading_the_shared_session(prefix: &str) {
    let src = read_in("src/channels/slack", "handler.rs");
    let lines: Vec<&str> = src.lines().collect();
    let arm = line_of(&lines, &format!("action_id.strip_prefix(\"{prefix}:\")"), 0)
        .unwrap_or_else(|| panic!("{HANDLER}: the {prefix}: arm is gone; re-check this test"));
    let gate = line_of(&lines, "may_act_on_shared_session(", arm)
        .unwrap_or_else(|| panic!("{HANDLER}: the {prefix}: arm has no admission gate (F-CH-02)"));
    let shared = line_of(&lines, "state.shared_session", arm).unwrap_or_else(|| {
        panic!("{HANDLER}: the {prefix}: arm no longer reads the shared session")
    });
    assert!(
        gate < shared,
        "{HANDLER}: the {prefix}: arm reads the shared session at line {} before \
         the admission gate at line {}, so a refused tap still reaches \
         swap_provider_for_session (F-CH-02)",
        shared + 1,
        gate + 1
    );
}

#[test]
fn the_provider_arm_gates_before_reading_the_shared_session() {
    assert_arm_gates_before_reading_the_shared_session("provider");
}

#[test]
fn the_model_arm_gates_before_reading_the_shared_session() {
    assert_arm_gates_before_reading_the_shared_session("model");
}

/// Fix the class, not the instance: the next arm that reads the shared session
/// has to fail this test rather than ship ungated. The `session:` arm is the
/// one other caller, and it authorizes inline with `is_owner`.
#[test]
fn every_shared_session_read_in_on_interaction_is_authorized_first() {
    let src = read_in("src/channels/slack", "handler.rs");
    let body = on_interaction_body(&src);

    let mut checked = 0;
    for (i, line) in body.iter().enumerate() {
        if !line.contains("state.shared_session") {
            continue;
        }
        let arm = line_before(&body, "action_id.strip_prefix(", i).unwrap_or_else(|| {
            panic!(
                "{HANDLER}: a shared-session write at line {} sits outside any \
                 action arm, so nothing authorizes it (F-CH-02)",
                i + 1
            )
        });
        let window = body[arm..i].join("\n");
        assert!(
            window.contains("may_act_on_shared_session(") || window.contains("is_owner("),
            "{HANDLER}: the arm at line {} reaches the shared session at line {} \
             with no identity check in between (F-CH-02)",
            arm + 1,
            i + 1
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "{HANDLER}: expected the provider, model and session arms to read the \
         shared session, found {checked}; the test's premise changed"
    );
}

/// One rule, not three: the shared-session gate must stay the owner rule the
/// neighbouring arms use. A second, more permissive rule written inline is how
/// the arms drifted apart in the first place.
#[test]
fn the_shared_session_gate_is_the_owner_rule() {
    let src = read_in("src/channels/slack", "handler.rs");
    let lines: Vec<&str> = src.lines().collect();
    let helper = line_of(&lines, "fn may_act_on_shared_session(", 0)
        .expect("the shared-session gate exists");
    let window = lines[helper..(helper + 4).min(lines.len())].join("\n");
    assert!(
        window.contains("sl_cfg.is_owner(user_id)"),
        "{HANDLER}: the shared-session gate must delegate to SlackConfig::is_owner, \
         the same rule the `session:` and approval arms apply"
    );
}
