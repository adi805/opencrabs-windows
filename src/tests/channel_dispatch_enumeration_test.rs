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

/// The byte range of the parameter schema: the top-level `"properties"` block.
///
/// Anchored on the indent-12 `"properties"` and `"required"` lines so that a
/// nested object schema (a button row, a select menu) cannot truncate it.
fn schema_range(src: &str) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    let mut start = None;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 12 && trimmed.starts_with("\"properties\"") {
            start = Some(offset);
        } else if start.is_some() && indent == 12 && trimmed.starts_with("\"required\"") {
            return Some((start?, offset));
        }
        offset += line.len();
    }
    None
}

/// The values of `key`'s `"enum": [ ... ]` inside the schema.
fn enum_values(src: &str, key: &str) -> Vec<String> {
    let Some((s, e)) = schema_range(src) else {
        return Vec::new();
    };
    let schema = &src[s..e];
    let needle = format!("\"{key}\": {{");
    let Some(at) = schema.find(&needle) else {
        return Vec::new();
    };
    let rest = &schema[at..];
    let Some(rel) = rest.find("\"enum\": [") else {
        return Vec::new();
    };
    let from = rel + "\"enum\": [".len();
    let Some(close) = rest[from..].find(']') else {
        return Vec::new();
    };
    quoted(&rest[from..from + close])
}

/// The body of a dispatch arm, from its `"name" => {` line to the next arm.
fn arm_body(src: &str, name: &str) -> String {
    let needle = format!("\"{name}\" =>");
    let Some(at) = src.find(&needle) else {
        return String::new();
    };
    let mut out = String::new();
    let mut seen_first = false;
    for line in src[at..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // The next arm, or the match's own closing brace, ends this one. A
        // nested `}` inside the arm body sits deeper than the arms, so testing
        // the indent (not just a leading brace) keeps it from truncating the
        // body early.
        if seen_first
            && ((indent == 12 && (trimmed.starts_with('"') || trimmed.starts_with('|')))
                || indent < 12)
        {
            break;
        }
        seen_first = true;
        out.push_str(line);
    }
    out
}

/// A sub-operation declared in an enum must have its own branch.
///
/// `debug_op` selects which runtime call `wa_introspect` makes, so every value
/// needs a match arm: a value with no arm is a declared, discoverable
/// operation that errors when the model asks for it. This is the nested form of
/// the reachability check above, and it is the shape that made
/// `wait_for_sent_node` look unreachable.
///
/// Value enums (`duration`, `spam_flow`, `event_response`) are deliberately not
/// swept this way: their unmatched values fall through to a documented default
/// (`_ => PinDuration::Days7`), which is a real behaviour rather than a hole.
#[test]
fn every_declared_sub_operation_has_a_branch() {
    let Some((_, src)) = TOOLS.iter().find(|(n, _)| *n == "whatsapp_send") else {
        panic!("whatsapp_send is in the tool table");
    };
    let values = enum_values(src, "debug_op");
    assert!(
        values.len() > 15,
        "parsed only {} debug_op values, so the schema scan stopped matching",
        values.len()
    );
    let body: String = arm_body(src, "wa_introspect")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let missing: Vec<&String> = values
        .iter()
        .filter(|v| {
            let q = format!("\"{v}\"");
            !(body.contains(&format!("{q}=>"))
                || body.contains(&format!("{q}|"))
                || body.contains(&format!("|{q}")))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "these wa_introspect operations are named in the schema with no branch to run them: \
         {missing:?}"
    );
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
