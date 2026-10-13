//! The shipped `tools.toml.example` must not teach command injection (SEC-01).
//!
//! A `{{param}}` in an UNQUOTED shell span is escaped for single quotes only
//! (`escape_for_shell_ctx`), so `;`, `|`, `&`, `$(...)`, backticks and spaces in
//! the value reach the shell as syntax. A param is model-supplied text, so an
//! unquoted interpolation in the example is a worked example of command
//! injection. Nothing read this file, so the shape could drift back silently;
//! this test is the thing that goes red if it does.

/// Every `{{name}}` in a shell command, as (byte index, name).
///
/// Skips the shapes that are not parameter interpolations: `{{.Names}}` is a
/// Docker format placeholder, and `{{#name}}` / `{{/name}}` are section tags.
fn shell_placeholders(cmd: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut search = 0usize;
    while let Some(rel) = cmd[search..].find("{{") {
        let at = search + rel;
        let rest = &cmd[at + 2..];
        let Some(end) = rest.find("}}") else { break };
        let name = &rest[..end];
        search = at + 2 + end + 2;
        if name.is_empty()
            || name.starts_with('.')
            || name.starts_with('#')
            || name.starts_with('/')
        {
            continue;
        }
        found.push((at, name));
    }
    found
}

/// Whether byte `at` sits inside a quoted span of `cmd`.
///
/// Counts quotes before the position: an odd count of either kind means the
/// placeholder is inside that quote, which is what the escaper needs to see.
fn inside_quotes(cmd: &str, at: usize) -> bool {
    let before = &cmd[..at];
    let double = before.matches('"').count();
    let single = before.matches('\'').count();
    double % 2 == 1 || single % 2 == 1
}

fn shell_commands() -> Vec<String> {
    let doc = std::fs::read_to_string("tools.toml.example")
        .expect("tools.toml.example is shipped at the crate root");
    let parsed: toml::Value = toml::from_str(&doc).expect("the example must be valid TOML");
    let tools = parsed
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("[[tools]] entries");
    tools
        .iter()
        .filter(|t| t.get("executor").and_then(|v| v.as_str()) == Some("shell"))
        .filter_map(|t| t.get("command").and_then(|v| v.as_str()).map(String::from))
        .collect()
}

#[test]
fn the_example_is_valid_toml() {
    // Reading it is the point: a malformed example is a broken copy-paste.
    assert!(
        !shell_commands().is_empty(),
        "no shell tools in the example"
    );
}

#[test]
fn every_shell_interpolation_in_the_example_is_quoted() {
    let mut checked = 0;
    for cmd in shell_commands() {
        for (at, name) in shell_placeholders(&cmd) {
            assert!(
                inside_quotes(&cmd, at),
                "unquoted {{{{{name}}}}} in tools.toml.example: {cmd:?}. An unquoted \
                 interpolation is escaped for single quotes only, so shell syntax in the \
                 value reaches the shell (SEC-01). Write \"{{{{{name}}}}}\" instead."
            );
            checked += 1;
        }
    }
    // The example exists to show the safe shape, so it must actually show it.
    assert!(
        checked >= 4,
        "expected the example to demonstrate at least 4 shell interpolations, saw {checked}"
    );
}

#[test]
fn the_example_states_the_quoting_rule() {
    let doc = std::fs::read_to_string("tools.toml.example").unwrap();
    assert!(
        doc.contains("QUOTE EVERY INTERPOLATION"),
        "the example must state the rule it demonstrates (SEC-01)"
    );
}

#[test]
fn the_placeholder_scanner_ignores_docker_and_section_tags() {
    // Guards the test itself: a scanner that counted these would demand quotes
    // around Docker's own format string and around section tags.
    let docker = r#"docker ps --format 'table {{.Names}}\t{{.Status}}'"#;
    assert!(shell_placeholders(docker).is_empty(), "{docker}");
    let section = "{{#ids}}--ids \"{{ids}}\"{{/ids}}";
    let names: Vec<&str> = shell_placeholders(section)
        .iter()
        .map(|(_, n)| *n)
        .collect();
    assert_eq!(names, vec!["ids"], "{section}");
}
