//! Admission for Discord components and modals (#212).
//!
//! Slash commands enter `handle_invoked_request`, which checks identity,
//! channel scope and the `dm_only` policy. `interaction_create` handled
//! followup buttons, `sel:` selections and `formsub:` modal submissions with no
//! such gate, and those paths CONSUME pending state (the TTL select and form
//! maps) and dispatch a turn. A user who could see a live shared component
//! could therefore start an agent turn or spend another pending input.
//!
//! These tests pin the decision table and, in source form, the ordering that
//! makes the gate meaningful: it must run BEFORE anything is consumed, because
//! authorizing afterwards still lets a refused tap spend the owner's pending
//! entry.

use crate::channels::discord::commands::{holds_allowed_role, identity_admitted};
use crate::config::types::DiscordConfig;
use crate::tests::source_shape::read_in;

const AGENT: &str = "src/channels/discord/agent.rs";
const INTERACTIONS: &str = "src/channels/discord/interactions.rs";

// ── the decision table ──────────────────────────────────────────────

#[test]
fn an_unconfigured_discord_channel_admits_nobody() {
    let dc = DiscordConfig::default();
    let unconfigured =
        dc.allowed_users.is_empty() && dc.allowed_roles.is_empty() && dc.bot_owner.is_empty();
    assert!(
        unconfigured,
        "fixture sanity: the default config is the unconfigured case"
    );
    assert!(
        !identity_admitted(unconfigured, false, false, false, false),
        "#212: an unconfigured channel is deny-by-default, exactly as the slash \
         command path already treated it"
    );
}

#[test]
fn each_admission_axis_grants_on_its_own() {
    // owner, allow list, allowed role, open channel: any one admits.
    assert!(identity_admitted(false, true, false, false, false));
    assert!(identity_admitted(false, false, true, false, false));
    assert!(identity_admitted(false, false, false, true, false));
    assert!(identity_admitted(false, false, false, false, true));
    assert!(
        !identity_admitted(false, false, false, false, false),
        "a configured channel with no matching grant refuses"
    );
}

#[test]
fn role_grants_are_compared_as_strings_not_ints() {
    assert!(holds_allowed_role(&["123".to_string()], &[123]));
    assert!(
        !holds_allowed_role(&["123".to_string()], &[124]),
        "an unrelated role must not admit"
    );
    assert!(
        !holds_allowed_role(&["123".to_string()], &[]),
        "no roles (a DM, or Discord omitting the member) grants nothing"
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

/// The component branch consumes pending state (`take_select`, `take_form`) and
/// dispatches turns. The gate must precede ALL of them.
#[test]
fn the_component_gate_precedes_every_consume_and_dispatch() {
    let src = read_in("src/channels/discord", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();

    let branch = line_of(
        &lines,
        "if let Some(comp) = interaction.message_component()",
        0,
    )
    .expect("the component branch exists");
    let gate = line_of(&lines, "admit_interaction(", branch)
        .unwrap_or_else(|| panic!("{AGENT}: the component branch has no admission gate (#212)"));

    for needle in [
        "take_select(",
        "take_form(",
        "route_followup_turn(",
        "route_interaction_turn(",
    ] {
        if let Some(idx) = line_of(&lines, needle, branch) {
            assert!(
                gate < idx,
                "{AGENT}: {needle} at line {} runs before the admission gate at \
                 line {}, so a refused tap can still consume pending state or \
                 dispatch a turn (#212)",
                idx + 1,
                gate + 1
            );
        }
    }
}

/// The modal branch is the other way into a turn, and it also consumes the form.
#[test]
fn the_modal_gate_precedes_take_form() {
    let src = read_in("src/channels/discord", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();

    let branch = line_of(&lines, "if let Interaction::Modal(modal) = &interaction", 0)
        .expect("the modal branch exists");
    let gate = line_of(&lines, "admit_interaction(", branch)
        .unwrap_or_else(|| panic!("{AGENT}: the modal branch has no admission gate (#212)"));
    let take = line_of(&lines, "take_form(form_id)", branch).expect("the modal consumes the form");

    assert!(
        gate < take,
        "{AGENT}: the modal consumes the pending form at line {} before the \
         admission gate at line {} (#212)",
        take + 1,
        gate + 1
    );
}

/// The command path must use the SAME function, or the three entry points drift
/// apart again and one of them silently loses a leg of the gate.
#[test]
fn all_three_entry_points_share_one_gate() {
    let inter = read_in("src/channels/discord", "interactions.rs");
    let agent = read_in("src/channels/discord", "agent.rs");

    assert!(
        inter.contains("pub(crate) async fn admit_interaction("),
        "{INTERACTIONS}: the shared gate must live here"
    );
    assert!(
        inter.contains("admit_interaction(&ctx.http, dc,"),
        "{INTERACTIONS}: the slash-command path must call the shared gate"
    );
    let component_uses = agent.matches("admit_interaction(").count();
    assert!(
        component_uses >= 2,
        "{AGENT}: expected the component AND modal branches to call the shared \
         gate, found {component_uses} call(s)"
    );
}

/// A refused interaction must be TOLD, not left spinning. Discord shows
/// "interaction failed" when nothing answers, which reads as a bot bug.
#[test]
fn a_refused_interaction_is_answered() {
    let src = read_in("src/channels/discord", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();

    for (branch_needle, label) in [
        (
            "if let Some(comp) = interaction.message_component()",
            "component",
        ),
        ("if let Interaction::Modal(modal) = &interaction", "modal"),
    ] {
        let branch = line_of(&lines, branch_needle, 0).expect("branch exists");
        let gate = line_of(&lines, "admit_interaction(", branch).expect("gate exists");
        // The refusal window runs from the gate to the next `return;`, which is
        // where the early exit is written.
        let ret = line_of(&lines, "return;", gate).expect("the gate returns early");
        let window = lines[gate..=ret].join("\n");
        assert!(
            window.contains("create_response"),
            "{AGENT}: the {label} refusal path does not answer the interaction, \
             so a refused user sees \"interaction failed\" instead of a reason"
        );
    }
}
