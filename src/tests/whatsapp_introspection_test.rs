//! Runtime introspection and tuning on the WhatsApp channel.
//!
//! The wrappers need a live socket, so the calls themselves cannot run here.
//! What is checkable without a connection is that each knob reaches the client
//! method it claims to, that a call with no session answers instead of
//! panicking, and that the raw-stanza lease is held rather than dropped: a
//! lease that fell out of scope would switch `Event::RawNode` off the instant
//! the call returned, which looks exactly like forwarding never worked.

const SOURCE: &str = include_str!("../channels/whatsapp/introspection.rs");
const STATE: &str = include_str!("../channels/whatsapp/state.rs");
const MOD: &str = include_str!("../channels/whatsapp/mod.rs");
const TOOL: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every knob this task wired, paired with the client call it must make.
///
/// The receiver is part of the pin: `set_resend_rate_limit` and
/// `set_retry_admission` both bound retry traffic but on different paths, and a
/// wrapper that reached for the wrong one would still compile.
const KNOBS: &[(&str, &str)] = &[
    ("raw-node lease", "acquire_raw_node_forwarding()"),
    ("session counters", ".stats()"),
    ("inbound waiter", ".wait_for_node("),
    ("outbound waiter", ".wait_for_sent_node("),
    ("resend limiter", ".set_resend_rate_limit("),
    ("history-sync skip", ".set_skip_history_sync("),
    ("pre-key batch", ".set_wanted_pre_key_count("),
    ("retry admission", ".set_retry_admission("),
];

/// The text with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn the_module_is_declared_beside_the_other_concerns() {
    // `mod.rs` is declarations only, and a module file that is never declared
    // compiles to nothing at all: the knobs would exist and be unreachable.
    assert!(
        squash(MOD).contains("modintrospection;"),
        "the introspection module must be declared in the channel mod.rs"
    );
}

#[test]
fn every_knob_reaches_the_client_method_it_names() {
    let compact = squash(SOURCE);
    for (label, call) in KNOBS {
        assert!(
            compact.contains(&squash(call)),
            "the {label} wrapper does not call `{call}`"
        );
    }
}

/// A debug call against a bot that is not connected is a normal answer, not a
/// fault: every wrapper asks for the client with `?` on the `Option` instead of
/// unwrapping it, so a disconnected session reports rather than panics.
#[test]
fn a_disconnected_session_is_answered_not_panicked_on() {
    let compact = squash(SOURCE);
    let guarded = compact.matches("self.client().await").count();
    assert!(
        guarded >= 8,
        "only {guarded} wrappers look up the live client; every knob must handle a \
         disconnected bot instead of assuming one"
    );
    assert!(
        !compact.contains("unwrap()"),
        "no wrapper may unwrap a missing client"
    );
}

/// `Event::RawNode` is enabled only while a lease is held, so the lease has to
/// outlive the call that acquired it. Dropping it at the end of the wrapper
/// would turn forwarding off again immediately.
#[test]
fn the_raw_node_lease_outlives_the_call_that_took_it() {
    let compact = squash(SOURCE);
    assert!(
        compact.contains("*slot=Some(lease)"),
        "the raw-node lease must be stored, not dropped at the end of the call"
    );
    assert!(
        squash(STATE)
            .contains("raw_node_lease:std::sync::Mutex<Option<whatsapp_rust::RawNodeLease>>"),
        "the state must be able to hold the lease"
    );
    assert!(
        squash(SOURCE).contains("slot.take().is_some()"),
        "releasing must report whether there was a lease to drop"
    );
}

/// The client accepts one retry-admission policy per session and ignores a
/// second registration, so a `false` has to surface as a refusal. Reporting
/// success would tell the operator a new limit is live while the old one is.
#[test]
fn a_second_retry_policy_is_reported_as_refused() {
    let compact = squash(SOURCE);
    assert!(
        compact.contains("set_retry_admission(policy)"),
        "the policy must be handed to the client"
    );
    assert!(
        compact.contains("Some(self.client().await?.set_retry_admission(policy))"),
        "the wrapper must pass the client's verdict through rather than assume success"
    );
}

/// A burst of zero is the off switch, matching the resend limiter's spelling.
/// A bucket that dropped every retry on `0` would silently quarantine a group
/// the operator had just asked to leave alone.
#[test]
fn a_zero_burst_admits_everything() {
    let compact = squash(SOURCE);
    assert!(
        compact.contains("ifself.burst==0{returntrue;}"),
        "a zero burst must admit every retry"
    );
    assert!(
        compact.contains("implRetryAdmissionforRetryBucket"),
        "the bucket must implement the policy trait the client registers"
    );
}

/// The tool action is how the knobs are reached without a rebuild, so it needs
/// both a schema entry (discoverable) and a match arm (reachable).
#[test]
fn the_tool_action_is_discoverable_and_reachable() {
    let compact = squash(TOOL);
    assert!(
        compact.matches("\"wa_introspect\"").count() >= 2,
        "wa_introspect needs a schema enum entry and a match arm"
    );
    for op in [
        "stats",
        "wait_for_node",
        "wait_for_sent_node",
        "acquire_raw_node",
        "release_raw_node",
        "set_resend_rate_limit",
        "set_skip_history_sync",
        "set_wanted_pre_key_count",
        "set_retry_admission",
    ] {
        assert!(
            compact.contains(&format!("\"{op}\"=>"))
                || compact.contains(&format!("\"{op}\"|"))
                || compact.contains(&format!("|{op}\"")),
            "debug_op '{op}' is named in the schema with no branch to run it"
        );
    }
}

/// An attribute narrows a filter, so half of one is a request the caller did
/// not make. Matching on the tag alone would report a node they never asked
/// about, which reads as the diagnostic agreeing with a wrong hypothesis.
#[test]
fn half_an_attribute_is_refused() {
    assert!(
        squash(TOOL).contains("'node_attr_key'and'node_attr_value'aresettogetheror\\notatall."),
        "a half-specified node attribute must be refused, not silently widened"
    );
}
