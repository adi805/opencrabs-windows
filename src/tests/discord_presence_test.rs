//! The bot's own presence (FR-003).
//!
//! `ActivityData` carries no `PartialEq` and there is no gateway in CI, so the
//! mapping is tested as text and the wiring is pinned in the source. A
//! regression here is silent by construction: the code still compiles, every
//! other test stays green, and the only symptom is a status line that never
//! moves, which nothing else in the suite can see.

use crate::channels::discord::presence::activity_text;

const AGENT: &str = include_str!("../channels/discord/agent.rs");
const HANDLER: &str = include_str!("../channels/discord/handler.rs");
const PRESENCE: &str = include_str!("../channels/discord/presence.rs");

/// The source with every run of whitespace collapsed to a single space, so a
/// check for a call is not defeated by where `rustfmt` chose to break the line.
fn flat(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn an_idle_bot_publishes_no_activity() {
    assert_eq!(
        activity_text(0),
        None,
        "idle must clear the activity: Discord keeps the last one until it is \
         changed, so an idle bot still advertising work looks hung (AC-005)"
    );
}

#[test]
fn a_running_turn_publishes_a_non_blank_activity() {
    let text = activity_text(1).expect("a turn in flight must publish an activity");
    assert!(
        !text.trim().is_empty(),
        "a blank activity is rendered as no status at all, so the change the \
         owner is supposed to see would be invisible (AC-005)"
    );
}

#[test]
fn the_activity_does_not_scale_with_the_number_of_turns() {
    // Presence is one value for the whole bot, so two channels running at once
    // must produce the same line as one. A count would flicker through numbers
    // the owner cannot act on.
    assert_eq!(activity_text(1), activity_text(7));
}

#[test]
fn the_ready_hook_reconciles_the_activity() {
    assert!(
        AGENT.contains("set_activity"),
        "`ready` must publish the activity. Discord keeps a presence until \
         something changes it, so a process that died mid-turn reconnects still \
         advertising work it is not doing, and `ready` is the only hook that \
         runs on every connect (AC-005)"
    );
}

#[test]
fn a_turn_holds_the_activity_for_its_whole_life() {
    // `let _ =` would drop the guard on the spot and flicker the status once per
    // turn, so the binding name is part of the contract, not a style choice.
    let handler = flat(HANDLER);
    assert!(
        handler.contains("let _presence_guard = super::presence::WorkingGuard::acquire("),
        "the turn must bind the presence guard to a live variable. Without it \
         nothing ever publishes the running state, so only `ready`'s idle line \
         is ever seen and the activity never changes with agent state (AC-005)"
    );
}

#[test]
fn publishing_the_activity_needs_no_intent() {
    // FR-003 is deliberately free of privileged intents: the bot's own status is
    // a gateway write, not a subscription. `GUILD_PRESENCES` is what a bot must
    // be granted to RECEIVE other members' presence updates, and requesting it
    // would make this feature depend on an owner-side Developer Portal toggle
    // for no gain (NFR-001).
    //
    // Comments are stripped before the scan because this module's own doc names
    // the intent it must not request: a check that read the prose explaining the
    // rule would fail on the explanation.
    let code: String = PRESENCE
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("GatewayIntents"),
        "the presence module must not request an intent: the bot's own status \
         needs none, and one here would silently turn FR-003 into a feature \
         that cannot work until the owner flips a Portal toggle"
    );
}

#[test]
fn the_guard_reverts_the_activity_when_it_drops() {
    // The other half of AC-005: a guard that publishes but never clears leaves
    // the bot advertising work after the turn ends, which reads as a hung bot.
    let source = flat(PRESENCE);
    assert!(
        source.contains("impl Drop for WorkingGuard"),
        "the guard must revert in its destructor: a turn has many return paths \
         including the error ones, and only `Drop` covers all of them, so an \
         explicit reset would be skipped on the paths that matter most"
    );
    assert!(
        source.contains("self.shard.set_activity(steady());"),
        "the destructor must republish the activity, which is what clears it \
         once the last turn ends (AC-005)"
    );
}

#[test]
fn the_scan_sees_the_module_it_governs() {
    // A source scan that reads an emptied or renamed file passes every absence
    // assertion above for the wrong reason. Pin that the module is really there
    // and really holds the machinery, so a refactor that moves it fails here
    // loudly instead of quietly disarming the intent guard.
    assert!(
        PRESENCE.contains("struct WorkingGuard") && PRESENCE.contains("static IN_FLIGHT"),
        "the scan above governs the presence module's own source, so it must \
         find the guard and the counter in it. If they moved, move this test"
    );
}
