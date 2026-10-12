//! An action that reads a parameter the schema never declares.
//!
//! `edit` read its body from a key named `text`, which the schema does not
//! declare: the documented surface is `message`. Every caller that followed the
//! schema got `Missing required parameter 'text'`, so the action could not be
//! invoked successfully at all, and no test noticed because none of them drove
//! the arm with a schema-shaped payload.
//!
//! `create_event` carried the same defect on three optional fields
//! (`join_link`, `is_scheduled_call`, `extra_guests_allowed`). There the failure
//! is quieter than a hard error: the value is simply always absent, so the
//! feature is unreachable rather than visibly broken.
//!
//! Reachability tests cover the opposite direction (a declared action with no
//! arm to run it). This covers the read direction: every key an arm pulls out of
//! its input must be one the schema told the model it could send.

use std::collections::BTreeSet;

/// The four channel tools, by the file that implements their schema.
const TOOLS: &[(&str, &str)] = &[
    ("whatsapp", "whatsapp_send.rs"),
    ("telegram", "telegram_send.rs"),
    ("discord", "discord_send.rs"),
    ("slack", "slack_send.rs"),
];

fn tool_source(file: &str) -> String {
    let path = format!("{}/src/brain/tools/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// Keys declared at the top level of the tool's parameter schema.
///
/// The block runs from the first `"properties"` indented twelve spaces to the
/// next `"required"` at that same indent. Nested object schemas (button rows,
/// select menus) declare their own `properties`/`required` pairs deeper in, so
/// both anchors are pinned to the top-level indent. Matching the first
/// `"required"` regardless of indent truncates the block at the first nested
/// object and invents missing keys.
fn declared_params(src: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = src.lines().collect();
    let indent_of = |l: &&str| l.len() - l.trim_start().len();
    let start = lines
        .iter()
        .position(|l| indent_of(l) == 12 && l.trim_start().starts_with("\"properties\""));
    let Some(start) = start else {
        return BTreeSet::new();
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| indent_of(l) == 12 && l.trim_start().starts_with("\"required\""))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    let mut out = BTreeSet::new();
    for line in &lines[start..end] {
        // Parameter keys sit at indent 16; the keywords inside each parameter
        // (`type`, `description`, `enum`) sit at 20 and deeper. Pinning the
        // indent to 16 keeps those keywords out of the set, which would
        // otherwise let an arm read a key named `type` and pass.
        if indent_of(line) != 16 {
            continue;
        }
        let Some(rest) = line.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some((key, _)) = rest.split_once("\":") else {
            continue;
        };
        if key == "enum" || key == "properties" {
            continue;
        }
        out.insert(key.to_string());
    }
    out
}

/// Every key the source pulls out of its `input` object.
///
/// Whitespace is stripped before scanning so that a read split across lines
/// (`input` then `.get("caption")`) is seen the same as one written inline.
/// Without that, the multi-line form reads as "never used" and hides the very
/// mismatch this test exists to catch.
fn read_params(src: &str) -> BTreeSet<String> {
    let squashed: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = BTreeSet::new();
    // `input.get("k")`, `input["k"]`, and the argument position of the
    // accessor helpers (`pget!(input, "k")`, `get_str(&input, "k")`), which all
    // squash to `input,"k"`.
    for marker in ["input.get(\"", "input[\"", "input,\""] {
        let mut rest = squashed.as_str();
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let Some(end) = rest.find('"') else { break };
            let key = &rest[..end];
            let is_param_key = !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if is_param_key {
                out.insert(key.to_string());
            }
        }
    }
    out
}

/// A guard on the reader: if a schema block stops being found, the assertions
/// below would pass without examining anything.
#[test]
fn every_tool_schema_is_readable() {
    for (name, file) in TOOLS {
        let src = tool_source(file);
        assert!(
            !src.is_empty(),
            "could not read the {name} tool source ({file})"
        );
        let declared = declared_params(&src);
        let reads = read_params(&src);
        assert!(
            declared.len() > 10,
            "{name}: parsed only {} declared params, so the schema block stopped matching",
            declared.len()
        );
        assert!(
            reads.len() > 5,
            "{name}: parsed only {} param reads, so the accessor scan stopped matching",
            reads.len()
        );
    }
}

#[test]
fn no_arm_reads_a_param_the_schema_does_not_declare() {
    let mut problems = Vec::new();
    for (name, file) in TOOLS {
        let src = tool_source(file);
        let declared = declared_params(&src);
        let reads = read_params(&src);
        let undeclared: Vec<&String> = reads.difference(&declared).collect();
        if !undeclared.is_empty() {
            problems.push(format!("{name}: {undeclared:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "an arm reads a parameter the schema never declares, so a caller following the schema can \
         never set it (a required one fails outright; an optional one is silently always absent):\n  {}",
        problems.join("\n  ")
    );
}
