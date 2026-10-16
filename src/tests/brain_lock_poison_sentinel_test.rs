//! A lock guard must survive a poisoned lock (F-BR-01).
//!
//! `src/brain` held 34 sites of `.lock().unwrap()` / `.read().unwrap()` /
//! `.write().unwrap()`. A poisoned lock stays poisoned, so the first panic
//! inside a critical section made every LATER access panic too: one transient
//! failure became a permanent one, on the provider and tool-registry paths a
//! turn cannot run without.
//!
//! The tolerant idiom `.unwrap_or_else(|e| e.into_inner())` was already used in
//! the same tree (`timezone.rs`, `restart_recovery.rs`), so the class was
//! consistency that had leaked, not a missing technique.
//!
//! This is a source sentinel rather than a behavioural test: the lock fields
//! are private, so a test outside their module cannot poison them, and adding a
//! test-only constructor to production types to reach them would be the wrong
//! trade. The walk covers the whole `src/brain` tree at run time, so a new file
//! is covered the day it lands rather than the day someone remembers to add it
//! to a list.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, recursively.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The shape that panics on a poisoned lock. `unwrap_or_else` and `expect` are
/// deliberately not matched: `expect` with a real message is a different
/// decision, and the tolerant idiom is what this pins.
fn panicking_lock_access(line: &str) -> Option<&'static str> {
    for accessor in [".lock()", ".read()", ".write()"] {
        let needle = format!("{accessor}.unwrap()");
        if line.contains(&needle) {
            return Some(accessor);
        }
    }
    None
}

#[test]
fn no_brain_source_panics_on_a_poisoned_lock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/brain");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(
        files.len() > 100,
        "the walk must reach the tree, found {} files under {}",
        files.len(),
        root.display()
    );

    let mut offences = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(accessor) = panicking_lock_access(line) {
                offences.push(format!(
                    "{}:{} {} unwrap on a poisoned lock",
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(path)
                        .display(),
                    idx + 1,
                    accessor
                ));
            }
        }
    }

    assert!(
        offences.is_empty(),
        "F-BR-01 REGRESSION: a poisoned lock panics instead of recovering, so one \
         panic inside a critical section takes the path down permanently. Use \
         `.unwrap_or_else(|e| e.into_inner())`:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_sentinel_recognises_the_shape_it_hunts() {
    // A gate that cannot fail is not a gate. The detector is exercised on the
    // exact strings it must catch and on the tolerant form it must not.
    assert_eq!(
        panicking_lock_access("let g = self.inner.lock().unwrap();"),
        Some(".lock()")
    );
    assert_eq!(
        panicking_lock_access("let g = self.tokens.read().unwrap();"),
        Some(".read()")
    );
    assert_eq!(
        panicking_lock_access("let g = self.state.write().unwrap();"),
        Some(".write()")
    );
    assert_eq!(
        panicking_lock_access("let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());"),
        None
    );
    assert_eq!(
        panicking_lock_access("let g = self.inner.lock().expect(\"lock\");"),
        None
    );
}
