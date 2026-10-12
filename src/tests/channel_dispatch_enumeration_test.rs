//! The dispatch table and the action list must name the same set.
//!
//! The reachability direction (declared -> arm) is already covered by
//! `readme_channel_surface_test`. The direction that was missing is the
//! reverse: an arm added to the dispatch table without an entry in the action
//! list. That arm runs, but the model never sees it, because the list is the
//! only thing it reads. This test derives BOTH sets from the source and
//! requires them to be equal, so neither direction can drift silently.
//!
//! Both sides are parsed, never listed: a hand-written list of names is exactly
//! what let the earlier collisions through.

use std::collections::BTreeSet;

const TOOLS: &[(&str, &str)] = &[
    (
        "whatsapp_send",
        include_str!("../brain/tools/whatsapp_send.rs"),
    ),
    (
        "telegram_send",
        include_str!("../brain/tools/telegram_send.rs"),
    ),
    (
        "discord_send",
        include_str!("../brain/tools/discord_send.rs"),
    ),
    ("slack_send", include_str!("../brain/tools/slack_send.rs")),
];

/// The double-quoted literals in a fragment, in order.
fn quoted(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find('"') {
        let open = from + rel + 1;
        match body[open..].find('"') {
            Some(close) => {
                out.push(body[open..open + close].to_string());
                from = open + close + 1;
            }
            None => break,
        }
    }
    out
}

/// The action list: the first `"enum": [ ... ]` array in a channel tool.
fn declared_actions(src: &str) -> Vec<String> {
    let Some(rel) = src.find("\"enum\": [") else {
        return Vec::new();
    };
    let at = rel + "\"enum\": [".len();
    let Some(end) = src[at..].find(']') else {
        return Vec::new();
    };
    quoted(&src[at..at + end])
}

/// The arms of the dispatch table.
///
/// Two helper functions in `discord_send.rs` also match on a variable named
/// `action` (an automod label and an audit-log label), so the block is chosen
/// by shape: the `match action` whose body holds the most quoted arms is the
/// dispatch table, the others hold none.
fn dispatch_arms(src: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut best: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if !t.starts_with("match action") || !t.ends_with('{') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let mut out: Vec<String> = Vec::new();
        let mut depth: i32 = 0;
        let mut pending = String::new();
        for (j, l) in lines.iter().enumerate().skip(i) {
            let cur = l.len() - l.trim_start().len();
            let tt = l.trim();
            if depth > 0 && cur == indent + 4 && (tt.starts_with('"') || tt.starts_with('|')) {
                match tt.find("=>") {
                    Some(rel) => {
                        pending.push_str(&tt[..rel]);
                        out.extend(quoted(&pending));
                        pending.clear();
                    }
                    None => pending.push_str(tt),
                }
            }
            depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
            if j > i && depth <= 0 {
                break;
            }
        }
        if out.len() > best.len() {
            best = out;
        }
    }
    best
}

/// A guard on the parsers: if either side stops matching, the equality
/// assertions below would compare two empty sets and pass.
#[test]
fn both_sides_parse() {
    for (name, src) in TOOLS {
        let declared = declared_actions(src);
        let arms = dispatch_arms(src);
        assert!(
            declared.len() > 10,
            "{name}: parsed only {} declared actions",
            declared.len()
        );
        assert!(
            arms.len() > 10,
            "{name}: parsed only {} dispatch arms",
            arms.len()
        );
    }
}

#[test]
fn every_dispatch_arm_is_a_declared_action() {
    for (name, src) in TOOLS {
        let declared: BTreeSet<String> = declared_actions(src).into_iter().collect();
        let mut orphans: Vec<String> = dispatch_arms(src)
            .into_iter()
            .filter(|arm| !declared.contains(arm))
            .collect();
        orphans.sort();
        orphans.dedup();
        assert!(
            orphans.is_empty(),
            "{name}: these arms run but are not in the action list, so the model cannot discover \
             them: {orphans:?}"
        );
    }
}

#[test]
fn every_declared_action_has_a_dispatch_arm() {
    for (name, src) in TOOLS {
        let arms: BTreeSet<String> = dispatch_arms(src).into_iter().collect();
        let mut missing: Vec<String> = declared_actions(src)
            .into_iter()
            .filter(|a| !arms.contains(a))
            .collect();
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "{name}: declared in the action list but with no arm to run it: {missing:?}"
        );
    }
}
