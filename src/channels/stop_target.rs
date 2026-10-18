//! Targeted stop: `/stop --session <id>` (#2108).
//!
//! Plain `/stop` cancels the session bound to the chat the command arrived
//! in. An operator watching a stuck turn in another topic had no way to
//! reach it, so this module owns the argument parsing, the id resolution and
//! the receipt wording for the targeted form. Everything here is pure: the
//! channel handlers do the cancelling and hand the observed state in.

use uuid::Uuid;

/// How a targeted stop landed, as reported back to the operator.
///
/// The three states exist because a session can be busy without holding a
/// live cancel handle (the handle is created when the turn starts and
/// removed when it ends, so a turn that is between phases, or one whose
/// token was already taken, is active but not directly cancellable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// A live cancel token existed and was cancelled: the running turn stops
    /// now.
    Delivered,
    /// The session is marked busy but held no live token, so the stop takes
    /// effect at the next turn boundary.
    Queued,
    /// Nothing was running for that session.
    Idle,
}

/// Classify a targeted stop from what the handler observed.
///
/// `had_token` is the return of the session cancel call; `was_active` is the
/// turn-active probe taken before cancelling (a delivered cancel also clears
/// the active marker, so probing afterwards would always read idle).
pub fn classify(had_token: bool, was_active: bool) -> CancelOutcome {
    match (had_token, was_active) {
        (true, _) => CancelOutcome::Delivered,
        (false, true) => CancelOutcome::Queued,
        (false, false) => CancelOutcome::Idle,
    }
}

/// Operator-facing receipt for a targeted stop.
///
/// The target is always named: an operator firing at a session id from
/// another topic needs to see which one answered, not just "stopped".
pub fn receipt(outcome: CancelOutcome, label: &str, id: Uuid) -> String {
    let label = if label.trim().is_empty() {
        "that session"
    } else {
        label.trim()
    };
    match outcome {
        CancelOutcome::Delivered => {
            format!("Stop delivered to {label} ({id}). The running turn is cancelled.")
        }
        CancelOutcome::Queued => format!(
            "Stop queued for {label} ({id}): the session is busy but holds no live cancel \
             handle, so it stops at the next turn boundary. Queued follow-ups were cleared."
        ),
        CancelOutcome::Idle => format!("{label} ({id}) is idle. Nothing to cancel."),
    }
}

/// Why a `/stop` argument list could not be turned into a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopArgError {
    /// `--session` with nothing after it, or arguments that named no target.
    MissingValue,
    /// A flag this command does not know, so the operator probably meant
    /// something else (`--session-id`, `--id`, ...).
    UnknownFlag(String),
}

/// Pull the target token out of a `/stop` argument list.
///
/// Accepts `--session <id>`, `-s <id>`, or a bare `<id>` (the last two are
/// convenience: typing the id alone is unambiguous). Anything else that
/// starts with a dash is refused rather than silently ignored, so a typo
/// does not degrade into a plain `/stop` on the wrong session.
pub fn parse_target(args: &str) -> Result<&str, StopArgError> {
    let mut tokens = args.split_whitespace();
    let mut target: Option<&str> = None;
    while let Some(tok) = tokens.next() {
        match tok {
            "--session" | "-s" => match tokens.next() {
                Some(value) => target = Some(value),
                None => return Err(StopArgError::MissingValue),
            },
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(StopArgError::UnknownFlag(other.to_string()));
            }
            other => target = Some(other),
        }
    }
    target.ok_or(StopArgError::MissingValue)
}

/// A resolved stop target plus the label used in the receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopTarget {
    pub id: Uuid,
    pub label: String,
}

/// Why a target token did not resolve to exactly one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// Shorter than the minimum prefix, so it could match almost anything.
    TooShort,
    /// Not a session id at all (wrong alphabet, or a title was passed).
    NotAnId(String),
    /// A well-formed prefix that matches nothing.
    NoMatch(String),
    /// A well-formed prefix that matches more than one session.
    Ambiguous { prefix: String, count: usize },
}

/// Shortest accepted id prefix. Four hex characters is 65k buckets, which
/// keeps a hand-typed prefix useful while making an accidental multi-match
/// the common failure instead of a silent wrong-target cancel.
pub const MIN_PREFIX: usize = 4;

/// Resolve a target token against the sessions the caller can see.
///
/// A full UUID is accepted without consulting `candidates`: the list is a
/// recent window, and refusing a correct id because it fell outside that
/// window would be worse than accepting it. Prefixes do need the list,
/// because that is the only thing that can decide between two candidates.
pub fn resolve(token: &str, candidates: &[(Uuid, String)]) -> Result<StopTarget, TargetError> {
    let token = token.trim();
    if let Ok(id) = Uuid::parse_str(token) {
        let label = candidates
            .iter()
            .find(|(cand, _)| *cand == id)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| "session".to_string());
        return Ok(StopTarget { id, label });
    }

    let needle = token.replace('-', "").to_ascii_lowercase();
    // Order matters: a title like "deploy watch" is refused as "not an id",
    // while "abc" is a well-formed hex prefix that is simply too short to
    // disambiguate. Reporting the wrong one of those sends the operator
    // looking for a typo that is not there.
    if needle.is_empty() || !needle.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TargetError::NotAnId(token.to_string()));
    }
    if needle.len() < MIN_PREFIX {
        return Err(TargetError::TooShort);
    }
    let matches: Vec<&(Uuid, String)> = candidates
        .iter()
        .filter(|(id, _)| {
            let plain = id.simple().to_string();
            plain.starts_with(&needle) || id.to_string().starts_with(&needle)
        })
        .collect();
    match matches.len() {
        0 => Err(TargetError::NoMatch(token.to_string())),
        1 => Ok(StopTarget {
            id: matches[0].0,
            label: matches[0].1.clone(),
        }),
        count => Err(TargetError::Ambiguous {
            prefix: token.to_string(),
            count,
        }),
    }
}

/// The exact command shape, repeated whenever a target is refused.
pub const USAGE: &str = "/stop --session <id> (or plain /stop for this session)";

/// Message for a malformed argument list.
pub fn usage_error(err: &StopArgError) -> String {
    match err {
        StopArgError::MissingValue => format!("Missing a session id. Usage: {USAGE}."),
        StopArgError::UnknownFlag(flag) => {
            format!("Unknown option \"{flag}\". Usage: {USAGE}.")
        }
    }
}

/// Message for a target that did not resolve.
pub fn target_error(err: &TargetError) -> String {
    match err {
        TargetError::TooShort => format!(
            "That id is too short to identify a session. Use at least {MIN_PREFIX} \
             characters, or copy the full id from /sessions."
        ),
        TargetError::NotAnId(token) => format!(
            "\"{token}\" is not a session id. Run /sessions and copy the id. Usage: {USAGE}."
        ),
        TargetError::NoMatch(token) => {
            format!("No session matches \"{token}\". Run /sessions to list ids.")
        }
        TargetError::Ambiguous { prefix, count } => format!(
            "\"{prefix}\" matches {count} sessions. Use more characters, or copy the full id \
             from /sessions."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn classify_prefers_delivered() {
        assert_eq!(classify(true, true), CancelOutcome::Delivered);
        assert_eq!(classify(true, false), CancelOutcome::Delivered);
        assert_eq!(classify(false, true), CancelOutcome::Queued);
        assert_eq!(classify(false, false), CancelOutcome::Idle);
    }

    #[test]
    fn receipt_names_the_target() {
        let target = id(0x2a);
        let text = receipt(CancelOutcome::Delivered, "deploy watch", target);
        assert!(text.contains("deploy watch"), "{text}");
        assert!(text.contains(&target.to_string()), "{text}");
        assert!(text.contains("cancelled"), "{text}");
        // No em dash: the house style guard covers new lines, and a receipt is
        // user-visible prose.
        assert!(!text.contains('\u{2014}'), "{text}");
    }

    #[test]
    fn receipt_falls_back_to_a_generic_label() {
        let text = receipt(CancelOutcome::Idle, "   ", id(1));
        assert!(text.starts_with("that session"), "{text}");
    }

    #[test]
    fn parse_accepts_session_flag() {
        assert_eq!(parse_target("--session 3f2a"), Ok("3f2a"));
        assert_eq!(parse_target("  -s   3f2a  "), Ok("3f2a"));
    }

    #[test]
    fn parse_accepts_bare_id() {
        assert_eq!(parse_target("3f2a-11"), Ok("3f2a-11"));
    }

    #[test]
    fn parse_refuses_missing_value_and_unknown_flags() {
        assert_eq!(parse_target(""), Err(StopArgError::MissingValue));
        assert_eq!(parse_target("--session"), Err(StopArgError::MissingValue));
        assert_eq!(
            parse_target("--id 3f2a"),
            Err(StopArgError::UnknownFlag("--id".into()))
        );
    }

    #[test]
    fn resolve_full_id_wins_even_outside_the_list() {
        let target = id(9);
        let resolved = resolve(&target.to_string(), &[]).expect("full id resolves");
        assert_eq!(resolved.id, target);
        assert_eq!(resolved.label, "session");
    }

    #[test]
    fn resolve_full_id_takes_the_label_from_the_list() {
        let target = id(9);
        let list = vec![(target, "nightly build".to_string())];
        let resolved = resolve(&target.to_string().to_uppercase(), &list).expect("resolves");
        assert_eq!(resolved.id, target);
        assert_eq!(resolved.label, "nightly build");
    }

    #[test]
    fn resolve_prefix_is_unique_or_refused() {
        let a = id(0xa1);
        let b = id(0xb1);
        let list = vec![(a, "first".to_string()), (b, "second".to_string())];
        let resolved = resolve("00000000-0000-0000-0000-0000000000a1", &list);
        assert!(resolved.is_ok(), "{resolved:?}");
        // Both share the "00000000-0000" head, so a short prefix is ambiguous.
        match resolve("00000000", &list) {
            Err(TargetError::Ambiguous { count, .. }) => assert_eq!(count, 2),
            other => panic!("expected ambiguous, got {other:?}"),
        }
        match resolve("abcdef", &list) {
            Err(TargetError::NoMatch(token)) => assert_eq!(token, "abcdef"),
            other => panic!("expected no match, got {other:?}"),
        }
    }

    #[test]
    fn resolve_rejects_short_and_non_hex_tokens() {
        let list = vec![(id(1), "one".to_string())];
        assert_eq!(resolve("abc", &list), Err(TargetError::TooShort));
        assert_eq!(
            resolve("deploy watch", &list),
            Err(TargetError::NotAnId("deploy watch".into()))
        );
    }

    #[test]
    fn errors_repeat_the_usage_line() {
        assert!(usage_error(&StopArgError::MissingValue).contains(USAGE));
        assert!(
            target_error(&TargetError::Ambiguous {
                prefix: "0000".into(),
                count: 3
            })
            .contains("3 sessions")
        );
    }
}
