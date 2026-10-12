//! A method defined twice for one type.
//!
//! `is_connected` was declared on `WhatsAppState` in both `connection.rs`
//! (returning `bool`, read from a local flag) and `introspection.rs` (returning
//! `Option<bool>`, asked of the client library). rustc rejected the duplicate
//! with E0592, and then every call site that named the method became ambiguous:
//! eight errors, most of them in files nobody had edited that day.
//!
//! The `impl` blocks for these types are spread across the channel module, so a
//! grep of the file being edited cannot see the collision. This test enumerates
//! every `impl` block in the module and fails when one type ends up with two
//! methods of the same name.
//!
//! Trait impls are skipped on purpose: an inherent method and a trait method may
//! legitimately share a name, and the failure this guards against (E0592) is
//! two INHERENT definitions.

use std::collections::BTreeMap;

/// Every `.rs` file in the WhatsApp channel module.
///
/// Read from the crate root rather than listed, so a file added later is
/// covered without anyone remembering to add it here.
fn module_sources() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/channels/whatsapp");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        out.push((name, text));
    }
    out.sort();
    out
}

/// `(type, method)` for every inherent `fn` inside an `impl <Type> { ... }`.
fn impl_methods(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut depth: i32 = 0;
    let mut impl_depth: i32 = 0;
    for line in src.lines() {
        let t = line.trim();
        let is_comment = t.starts_with("//");
        if current.is_none()
            && !is_comment
            && (t.starts_with("impl ") || t.starts_with("impl<"))
            && t.contains('{')
        {
            let head = t
                .split('{')
                .next()
                .unwrap_or("")
                .split(" where ")
                .next()
                .unwrap_or("");
            // A trait impl names two types; only the inherent form matters here.
            if !head.contains(" for ") {
                let ty = head
                    .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .find(|s| !s.is_empty())
                    .unwrap_or("");
                if !ty.is_empty() {
                    current = Some(ty.to_string());
                    impl_depth = depth;
                }
            }
        }
        if !is_comment {
            let toks: Vec<&str> = t.split_whitespace().collect();
            if let Some(ty) = current.as_ref()
                && let Some(i) = toks.iter().position(|x| *x == "fn")
                && let Some(raw) = toks.get(i + 1)
            {
                let name: String = raw
                    .split(|c: char| c == '(' || c == '<' || c == ':')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    out.push((ty.clone(), name));
                }
            }
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if current.is_some() && depth <= impl_depth {
            current = None;
        }
    }
    out
}

/// A guard on the reader: if the module stops being found, the assertions below
/// would pass without examining anything.
#[test]
fn the_whatsapp_module_is_readable() {
    let sources = module_sources();
    assert!(
        sources.len() > 20,
        "expected more than 20 source files in the WhatsApp channel module, found {}",
        sources.len()
    );
    let methods: usize = sources.iter().map(|(_, s)| impl_methods(s).len()).sum();
    assert!(
        methods > 50,
        "parsed only {methods} impl methods, so the parser stopped matching"
    );
}

#[test]
fn no_method_is_defined_twice_for_one_type() {
    let sources = module_sources();
    let mut seen: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (file, src) in &sources {
        for (ty, method) in impl_methods(src) {
            seen.entry((ty, method)).or_default().push(file.clone());
        }
    }
    let mut problems = Vec::new();
    for ((ty, method), files) in &seen {
        if files.len() > 1 {
            problems.push(format!(
                "{ty}::{method} defined {} times ({})",
                files.len(),
                files.join(", ")
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the same method is defined more than once for one type. That is E0592, and it makes every \
         call site that names the method ambiguous, in files the change never touched:\n  {}",
        problems.join("\n  ")
    );
}
