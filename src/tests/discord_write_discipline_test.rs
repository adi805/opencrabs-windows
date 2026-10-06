//! FR-003 / NFR-001 / AC-024: no Discord write bypasses the governor.
//!
//! `src/channels/discord/writes.rs` is the single choke point for channel
//! writes. Every `ChannelId::say`, `send_message` and `edit_message` goes
//! through it, so the write budget, the 429 ladder and the telemetry see all
//! the traffic. That is a rule about the shape of the source, and a doc
//! comment cannot fail a build, so this test reads the sources and turns a
//! raw call into a build failure.
//!
//! Scope, stated so a later reader does not "fix" it by widening or narrowing:
//!
//! - Governed here: `ChannelId` writes, meaning new messages and edits.
//! - Deliberately NOT governed: interaction responses (`create_response`,
//!   `Defer`, `UpdateMessage`, `Acknowledge`, `Modal`). Discord gives an
//!   interaction three seconds to answer; a token bucket that holds a response
//!   past that deadline converts a throttle into a failed interaction. Those
//!   calls keep their own path, and the third test below pins that decision.
//!
//! Needles are receiver-shaped (`.say(`) and whitespace is stripped before
//! matching, so a call split across lines still counts. Prose in a comment that
//! merely names a symbol does not: a needle carries the receiver dot and the
//! opening paren.

use std::path::Path;

/// Receiver-shaped needles for a Discord channel write.
const NEEDLES: &[&str] = &[".say(", ".send_message(", ".edit_message("];

/// The governor's own implementation: the one file these may appear in.
const CHOKE_POINT: &str = "writes.rs";

/// Call sites that match a needle but are not Discord channel writes.
///
/// `Agent::send_message` is the internal agent API (`session, prompt, None`).
/// It never reaches Discord's HTTP layer, so the governor has nothing to say
/// about it and governing it would be cargo cult.
const ALLOWED: &[(&str, &str)] = &[("agent.rs", "agent_clone.send_message(")];

fn discord_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord")
}

/// Every `.rs` file in the Discord module, sorted, as `(name, flattened source)`.
///
/// Flattening removes all whitespace so `msg\n  .channel_id\n  .say(` reads as
/// `msg.channel_id.say(`.
fn sources() -> Vec<(String, String)> {
    let dir = discord_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let src =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
            (name, flat)
        })
        .collect()
}

/// `(file, needle, count)` for every raw channel write left outside the
/// governor, after subtracting the documented allowances.
fn raw_write_hits() -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for (name, flat) in sources() {
        for needle in NEEDLES {
            let hits = flat.matches(needle).count();
            let allowed: usize = ALLOWED
                .iter()
                .filter(|(f, _)| *f == name)
                .map(|(_, n)| flat.matches(n).count())
                .sum();
            let net = hits.saturating_sub(allowed);
            if net > 0 {
                out.push((name.clone(), (*needle).to_string(), net));
            }
        }
    }
    out
}

#[test]
fn no_raw_channel_write_survives_outside_the_governor() {
    let offenders: Vec<String> = raw_write_hits()
        .into_iter()
        .filter(|(file, _, _)| file != CHOKE_POINT)
        .map(|(file, needle, n)| format!("  src/channels/discord/{file}: {n}x {needle}"))
        .collect();
    assert!(
        offenders.is_empty(),
        "raw Discord channel writes bypass the governor (AC-024). Route them \
         through writes::say / writes::send / writes::edit so the budget, the \
         429 ladder and the telemetry see them:\n{}",
        offenders.join("\n")
    );
}

/// The scan must be able to fail. A path typo, a rename, or an empty needle
/// list would make the test above pass while checking nothing, which is worse
/// than no test: it reports safety it never verified.
#[test]
fn the_scan_sees_the_files_it_governs() {
    let seen = sources();
    assert!(
        seen.len() >= 15,
        "expected the whole Discord module, saw {} .rs files: {}",
        seen.len(),
        seen.iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let choke = seen
        .iter()
        .find(|(n, _)| n == CHOKE_POINT)
        .unwrap_or_else(|| panic!("{CHOKE_POINT} is not in the module any more"));
    for needle in NEEDLES {
        assert!(
            choke.1.contains(needle),
            "{CHOKE_POINT} no longer calls {needle}, so the scan would pass \
             vacuously: the governor is not writing anything"
        );
    }
}

/// Interaction responses stay outside the governor on purpose, and that is a
/// decision on record rather than an oversight. Discord's 3-second handshake
/// means a response that waits on a token bucket becomes a failed interaction,
/// so throttling them would trade a rare rate-limit warning for a user-visible
/// break. If these calls ever disappear, revisit the reasoning above.
#[test]
fn interaction_responses_are_deliberately_outside_the_governor() {
    let seen = sources();
    let agent = seen
        .iter()
        .find(|(n, _)| n == "agent.rs")
        .map(|(_, s)| s)
        .expect("agent.rs must exist: it owns the interaction responses");
    assert!(
        agent.contains("create_response("),
        "no interaction response left in agent.rs: re-check the 3-second \
         handshake reasoning documented at the top of this file"
    );
    assert!(
        agent.contains("CreateInteractionResponse::"),
        "interaction response construction moved: re-check the 3-second \
         handshake reasoning documented at the top of this file"
    );
}
