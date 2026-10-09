//! Send-correlation telemetry for the Telegram surface (#1085).

use crate::channels::telegram::telemetry::*;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn hash8_is_stable_and_8_hex_chars() {
    let a = content_hash8("hello world");
    let b = content_hash8("hello world");
    assert_eq!(a, b);
    assert_eq!(a.len(), 8);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash8_separates_different_content() {
    assert_ne!(content_hash8("hello world"), content_hash8("hello worlD"));
}

#[test]
fn hash8_handles_empty_and_multibyte() {
    assert_eq!(content_hash8("").len(), 8);
    // PT-PT and emoji content must not panic (multibyte boundary safety).
    assert_eq!(content_hash8("ção 🦀 açúcar").len(), 8);
}

// ---------------------------------------------------------------------------
// #721 (E1): outbound-REQUEST telemetry — typing / delete / reaction.
//
// These lines answer a different question from the landing lines: not "what
// reached the chat" but "what did we put ON THE WIRE". A request that fails is
// still a request, so the line is emitted before the API call and its presence
// says nothing about the outcome. The field order is the contract — a reader
// parses by position — so it is pinned exactly.
// ---------------------------------------------------------------------------

#[test]
fn request_line_pins_the_field_order_for_a_targeted_request() {
    let line = request_line(
        "turn",
        "typing loop",
        "-",
        "delete",
        "deleteMessage",
        -1001234567890,
        Some(7),
        Some(42),
    );
    assert_eq!(
        line,
        "Telegram request: origin=turn detail=typing loop session=- kind=delete path=deleteMessage chat=-1001234567890 thread=Some(7) msg=42"
    );
}

/// A chat action targets no message. `msg` is then the create-shaped dash
/// (the convention #676 established for rich creates), NOT an absent field:
/// every line carries every field, so a reader never has to guess a shape.
#[test]
fn request_line_uses_the_create_shape_when_a_request_targets_no_message() {
    let line = request_line(
        "turn",
        "typing tick",
        "-",
        "typing",
        "sendChatAction",
        -1001234567890,
        None,
        None,
    );
    assert!(
        line.ends_with("chat=-1001234567890 thread=None msg=-"),
        "a chat action targets no message, so msg must be the create-shaped dash; got: {line}"
    );
}

/// `session` is the originating session id where the site knows it and `-`
/// where it genuinely cannot — the existing `log_send_success` convention,
/// unchanged.
#[test]
fn request_line_keeps_the_dash_session_fallback() {
    let line = request_line(
        "system",
        "probe_topic",
        "-",
        "typing",
        "sendChatAction",
        555,
        Some(3),
        None,
    );
    assert!(line.contains("session=- kind=typing"), "got: {line}");
}

/// A request has no body, so the two body-describing fields of the send
/// schema (`len`, `hash8`) are DROPPED rather than carried as dead dashes.
#[test]
fn request_line_carries_no_message_body_fields() {
    let line = request_line(
        "tool",
        "delete",
        "-",
        "delete",
        "deleteMessage",
        1,
        None,
        Some(2),
    );
    assert!(!line.contains("len="), "a request has no body; got: {line}");
    assert!(
        !line.contains("hash8="),
        "a request has no body; got: {line}"
    );
}

/// The prefix is what makes a request greppable WITHOUT matching a landing,
/// and vice versa: `oc-log-search 'Telegram request:'` must never return a
/// send that landed.
#[test]
fn request_line_prefix_cannot_be_confused_with_a_landing_line() {
    let line = request_line(
        "turn",
        "typing loop",
        "-",
        "typing",
        "sendChatAction",
        1,
        None,
        None,
    );
    assert!(line.starts_with("Telegram request: "), "got: {line}");
    assert!(!line.contains("Telegram send ok:"), "got: {line}");
    assert!(!line.contains("Telegram send failed:"), "got: {line}");
}

// ---------------------------------------------------------------------------
// Emitted half: the line must actually reach the log at INFO.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct EventCapture {
    events: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl EventCapture {
    fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }
}

impl<S: tracing::Subscriber> Layer<S> for EventCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push((
            event.metadata().level().to_string(),
            visitor.message.unwrap_or_default(),
        ));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

/// INFO, not DEBUG — and the reason is the READER, not the file: `oc-log-search`
/// stages `grep -E ' (INFO|WARN|ERROR) '` by default, so a `debug!` request line
/// would be invisible to the ordinary read and E1 would swap one blind spot for
/// another (owner challenge 2026-09-30, re-derived at source).
#[test]
fn log_request_emits_exactly_one_info_line_under_the_request_prefix() {
    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());

    tracing::subscriber::with_default(subscriber, || {
        // Another test in this binary installs a process-wide subscriber capped
        // at WARN (`governor_gates_test.rs::ensure_tracing_capture`), which
        // would disable an INFO callsite if that dispatcher were the only one
        // consulted. It is not: building this thread's dispatch runs
        // `Dispatch::new` -> `callsite::register_dispatch`, which pushes THIS
        // subscriber into the dispatcher list and rebuilds the interest cache
        // over all of them. Two dispatchers that disagree combine to
        // `Interest::sometimes` (tracing-core `Interest::and`: differing
        // interests -> sometimes), never `never`, so the event is offered to
        // this thread's subscriber and captured. No manual interest rebuild is
        // needed -- and none is reachable from `tracing` 0.1 anyway.
        log_request(
            "turn",
            "typing loop",
            "-",
            "typing",
            "sendChatAction",
            -1001234567890,
            None,
            None,
        );
    });

    let events = capture.events();
    assert_eq!(
        events.len(),
        1,
        "exactly one request line must be emitted; got {events:?}"
    );
    assert_eq!(
        events[0].0, "INFO",
        "the request line must be emitted at INFO"
    );
    assert!(
        events[0]
            .1
            .contains("Telegram request: origin=turn detail=typing loop"),
        "the emitted line must carry the request prefix and its fields; got: {}",
        events[0].1
    );
}

// ---------------------------------------------------------------------------
// The positional shape of a landing line (#1085 P1a).
//
// The landing lines (`log_send_success`, `log_send_failure`, `log_request`) take
// `session` and `kind` as adjacent `&str` parameters, so swapping them compiles
// cleanly and no behavioural test notices: both orders emit a line. It shipped
// swapped at 57 call sites in `src/brain/tools/telegram_send.rs`, where
// `session` carried the action name and `kind` carried the session uuid. Every
// landing from the tool path was then attributed to a session called `edit`,
// which is the one question the schema exists to answer.
//
// The rule, enforced below: argument 3 is a session id or the `-` fallback,
// and argument 4 never names a session.
// ---------------------------------------------------------------------------

/// Every `.rs` file under `dir`, minus the test tree.
///
/// The needles this scan looks for are string literals in this very file, so a
/// scan that read the test tree would find its own needle list and govern
/// nothing.
fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("source tree is readable");
    for entry in entries {
        let path = entry.expect("directory entry").path();
        let dir_name = path.file_name().and_then(|n| n.to_str());
        if path.is_dir() && dir_name != Some("tests") {
            rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// The arguments of a call, given everything from just after its `(`.
///
/// Depth-aware and string-aware: a comma inside a nested call or inside a
/// string literal is content, not a separator.
fn call_args(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if start.is_none() {
                    start = Some(i);
                }
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'(' | b'[' | b'{' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b')' | b']' | b'}' => {
                if depth == 0 {
                    if let Some(s) = start {
                        args.push(src[s..i].trim().to_string());
                    }
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                if let Some(s) = start.take() {
                    args.push(src[s..i].trim().to_string());
                }
                i += 1;
                continue;
            }
            b if !b.is_ascii_whitespace() && start.is_none() => {
                start = Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    args
}

/// Argument 3 is the session and argument 4 the kind, at every landing line in
/// the tree.
#[test]
fn send_telemetry_never_swaps_session_and_kind() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);

    let mut checked = 0usize;
    for path in &files {
        let src = std::fs::read_to_string(path).expect("source file is readable");
        for needle in ["log_send_success(", "log_send_failure(", "log_request("] {
            let mut from = 0usize;
            while let Some(at) = src[from..].find(needle) {
                let open = from + at + needle.len();
                from = open;
                if src[..open - needle.len()].ends_with("fn ") {
                    continue; // the definition, not a call site
                }
                let args = call_args(&src[open..]);
                assert!(
                    args.len() >= 5,
                    "{}: {needle} call with only {} arguments",
                    path.display(),
                    args.len()
                );
                let (session, kind) = (args[2].as_str(), args[3].as_str());
                assert!(
                    !kind.contains("session"),
                    "{}: `kind` is `{kind}`, which carries the session; the two are swapped",
                    path.display()
                );
                assert!(
                    session == "\"-\"" || session.contains("session"),
                    "{}: `session` is `{session}`, not a session id or the `-` fallback",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 50, "the scan found only {checked} call sites");
}

// ---------------------------------------------------------------------------
// #1085 P1a follow-up: the `origin` slot is a CLOSED VOCABULARY.
//
// `origin` and `origin_detail` sit adjacent (args 1-2 at the landing lines),
// which is exactly the shape that let `session` and `kind` be swapped without
// a compile error. `telemetry.rs` declares the vocabulary as
// `turn | tool | cron | system`, with free text belonging in `origin_detail`
// at the call site.
//
// The wrapper family is DERIVED from fn signatures, never hand-listed: a new
// wrapper that forwards the pair is governed the day it lands, and a signature
// that declares the pair reversed fails here instead of at a log reader's desk.
// ---------------------------------------------------------------------------

/// The vocabulary a literal may hold in the `origin` slot.
const ORIGIN_VOCAB: [&str; 4] = ["\"turn\"", "\"tool\"", "\"cron\"", "\"system\""];

/// The parameter names of the `fn` starting at `from` (the byte after `fn `),
/// in declaration order. Generics are skipped, so `fn f<C>(` reads as `f`.
fn fn_params_at(src: &str, from: usize) -> Option<(String, Vec<String>)> {
    let name_end = src[from..]
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| from + i)
        .unwrap_or(src.len());
    let name = src[from..name_end].to_string();
    if name.is_empty() {
        return None;
    }
    let mut at = name_end;
    if at < src.len() && src[at..].starts_with('<') {
        let mut depth = 0usize;
        for (i, c) in src[at..].char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        at += i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    if at >= src.len() {
        return None;
    }
    let open = at + src[at..].find('(')?;
    let mut params = Vec::new();
    for arg in call_args(&src[open + 1..]) {
        // `origin: &str` reads as `origin`; `mut thread_id: Option<ThreadId>`
        // reads as `thread_id`.
        let head = arg.split(':').next().unwrap_or("").trim();
        let last = head.split_whitespace().last().unwrap_or("").trim();
        params.push(last.to_string());
    }
    Some((name, params))
}

/// A wrapper that forwards the `origin`/`origin_detail` pair: its name and the
/// two parameter positions.
type Wrapper = (String, usize, usize);

/// A source file and its text.
type Source = (std::path::PathBuf, String);

/// The `origin`/`origin_detail` wrapper family in `root`, plus every source
/// file read.
///
/// Returns `(family, files, declarations_reversed)`; a declaration whose two
/// parameters are not adjacent (or not `origin` first) is reported, not
/// silently accepted, because order is the whole contract.
fn origin_family(root: &std::path::Path) -> (Vec<Wrapper>, Vec<Source>, Vec<String>) {
    let mut files = Vec::new();
    rust_sources(root, &mut files);

    let mut family: Vec<Wrapper> = Vec::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut reversed = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("source file is readable");
        let mut cursor = 0usize;
        while let Some(rel) = src[cursor..].find("fn ") {
            let start = cursor + rel;
            cursor = start + 3;
            let line_start = src[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            if src[line_start..start].contains("//") {
                continue; // a mention in prose, not a declaration
            }
            let Some((name, params)) = fn_params_at(&src, cursor) else {
                continue;
            };
            let origin_at = params.iter().position(|p| p.as_str() == "origin");
            let detail_at = params.iter().position(|p| p.as_str() == "origin_detail");
            let (Some(o), Some(d)) = (origin_at, detail_at) else {
                continue;
            };
            if d != o + 1 {
                reversed.push(format!(
                    "{}: `{name}` declares origin at {o} and origin_detail at {d}; \
                     the pair must be adjacent with `origin` first",
                    path.display()
                ));
            }
            family.push((name, o, d));
        }
        sources.push((path.clone(), src));
    }
    (family, sources, reversed)
}

/// Every call site of the family in a tree, checked against the vocabulary.
///
/// Returns `(call sites checked, violations)`; each violation is a rendered
/// sentence naming the file, the wrapper and the offending value.
fn origin_vocab_violations(root: &std::path::Path) -> (usize, Vec<String>) {
    let (family, sources, reversed) = origin_family(root);
    let mut checked = 0usize;
    let mut violations = reversed;
    for (path, src) in &sources {
        for (name, o, d) in &family {
            let needle = format!("{name}(");
            let mut cursor = 0usize;
            while let Some(rel) = src[cursor..].find(needle.as_str()) {
                let start = cursor + rel;
                let open = start + needle.len();
                cursor = open;
                if src[..start].ends_with("fn ") {
                    continue; // the definition, not a call site
                }
                let line_start = src[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
                if src[line_start..start].contains("//") {
                    continue; // a mention in prose, not a call
                }
                let args = call_args(&src[open..]);
                if args.len() <= *d {
                    continue; // not a full call, so nothing to check
                }
                let origin_arg = args[*o].as_str();
                let detail_arg = args[*d].as_str();
                if !ORIGIN_VOCAB.contains(&origin_arg) && origin_arg != "origin" {
                    violations.push(format!(
                        "{}: `{name}` gets origin `{origin_arg}`, outside the closed \
                         vocabulary turn|tool|cron|system",
                        path.display()
                    ));
                }
                if ORIGIN_VOCAB.contains(&detail_arg) {
                    violations.push(format!(
                        "{}: `{name}` gets detail `{detail_arg}`, a vocabulary word; \
                         origin and origin_detail are swapped",
                        path.display()
                    ));
                }
                checked += 1;
            }
        }
    }
    (checked, violations)
}

/// The `origin` slot carries only `turn | tool | cron | system` (or the
/// pass-through identifier `origin`), and the `origin_detail` slot never
/// carries a vocabulary word: that shape is the swap.
#[test]
fn origin_slot_carries_only_the_closed_vocabulary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (checked, violations) = origin_vocab_violations(&root);
    assert!(
        violations.is_empty(),
        "origin/origin_detail vocabulary violations:\n{}",
        violations.join("\n")
    );
    assert!(checked > 100, "the scan found only {checked} call sites");
}
