//! Shared helpers for **source-shape tests**: tests that read a production
//! source file as text and match a needle against it instead of calling the
//! code.
//!
//! The practice is legitimate only where the decision is genuinely unreachable
//! from a unit test: a doc comment cannot fail a build when someone drops the
//! strip, and these can. The bound, and the argument a file header has to make
//! to qualify, are stated in `CONTRIBUTING.md` ("Where Tests Live").
//!
//! What is not legitimate is every file carrying its own copy of the
//! flattener. Before #197 each of these files did, and only one collapsed the
//! trailing comma rustfmt inserts before a closing delimiter it wrapped. The
//! other seven matched `f(a)` against `f(a,)` only by luck of line width, so
//! the same needle passed or failed depending on whether the formatter
//! happened to wrap the line (F5 in `AUDIT-v0.5.10.md`). There is exactly ONE
//! flattener now. Do not copy it back into a test file; import it.

use std::path::PathBuf;

/// The crate root, as the compiler sees it.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a file named relative to the crate root.
pub fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Read a file named relative to a directory under the crate root.
pub fn read_in(dir: &str, name: &str) -> String {
    let path = root().join(dir).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir` (relative to the crate root) as
/// `(file name, contents)`, sorted by file name.
pub fn sources_in(dir: &str) -> Vec<(String, String)> {
    let dir = root().join(dir);
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        out.push((name, std::fs::read_to_string(&path).expect("read source")));
    }
    out.sort();
    out
}

/// Flatten source for needle matching.
///
/// Whitespace first, then the trailing comma rustfmt inserts before a closing
/// delimiter it wrapped onto its own line: `f(a,)` and `f(a)` are the same
/// call, `[a,]` and `[a]` the same literal. Only the source is flattened; a
/// needle is written the way the code reads on one line, without the comma.
/// Without this the same call matches or not depending on whether rustfmt
/// happened to wrap it, which is the reflow that makes a source-pinning test
/// lie (#1880, F5 in `AUDIT-v0.5.10.md`).
///
/// Takes anything that derefs to `&str` so a caller that already owns the
/// source (`flatten(read("src/…"))`) and one that borrows a literal
/// (`flatten("f(a,)")`) are both written the obvious way: the call shape is
/// not the point of the test, the needle is.
pub fn flatten(src: impl AsRef<str>) -> String {
    let src = src.as_ref();
    let stripped: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    stripped
        .replace(",)", ")")
        .replace(",]", "]")
        .replace(",}", "}")
}

#[test]
fn flatten_collapses_the_trailing_comma_rustfmt_inserts() {
    // The F5 shape: rustfmt wraps a long call and leaves a trailing comma
    // before the closing paren. The needle is written without it, the way the
    // call reads on one line.
    let wrapped = "let rows = pager_row(mid, 0, chunks.len(),);";
    let single = "let rows = pager_row(mid, 0, chunks.len());";
    assert_eq!(flatten(wrapped), flatten(single));
    assert!(flatten(wrapped).contains("pager_row(mid,0,chunks.len())"));
}

#[test]
fn flatten_collapses_a_wrapped_collection_literal() {
    assert!(flatten("let v = vec![\n    a,\n    b,\n];").contains("vec![a,b]"));
    assert!(flatten("let s = Foo {\n    a,\n    b,\n};").contains("Foo{a,b}"));
}

#[test]
fn flatten_does_not_erase_a_real_difference() {
    // Negative case: the transform must not be a blunt "delete every comma".
    assert_ne!(flatten("writes::send(a)"), flatten("writes::send(b)"));
    assert_ne!(flatten("f(a,b)"), flatten("f(b,a)"));
    // Near-miss from another subject: an ordinary argument separator is not a
    // trailing comma and must survive untouched.
    assert_eq!(flatten("f(a, b)"), flatten("f(a,b)"));
    assert!(flatten("f(a, b)").contains("f(a,b)"));
}

#[test]
fn a_needle_written_with_whitespace_still_misses() {
    // The boundary, stated so nobody expects more than the helper promises:
    // only the SOURCE is flattened. Nothing flattens the needle, so a needle
    // carrying whitespace never matches.
    let src = "writes::\n    send(id, text);";
    assert!(flatten(src).contains("writes::send(id,text)"));
    assert!(!flatten(src).contains("writes::send(id, text)"));
}
