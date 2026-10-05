//! Additive key-level merge for `.toml` templates (#819).
//!
//! Brain files are prose and merge by appending sections the local copy lacks.
//! TOML cannot: appending an upstream `[providers.qwen]` block beside a local
//! one produces a duplicate key, and the file stops parsing. Doing that to
//! `usage_pricing.toml` would take the user's pricing config offline entirely.
//!
//! So upstream contributes only what is MISSING. A value the user already has
//! is never touched, because their copy may be a deliberate customisation: a
//! negotiated price, a self-hosted endpoint, a trimmed model list. Upstream
//! knows about new models; it does not know better than the user about the
//! ones they already configured.
//!
//! Motivating case: #816 and #817 added pricing for two models. Users needed
//! those rows and nothing else, while keeping every rate they had already set.

use std::collections::HashSet;

use toml_edit::{DocumentMut, Item, Table};

/// What a merge changed, for the caller's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Dotted paths added, e.g. `providers.qwen.entries`.
    pub added: Vec<String>,
}

impl MergeReport {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
    }
}

/// Merge `upstream` into `local`, adding only keys `local` lacks.
///
/// Returns the merged document and what was added. Formatting and comments in
/// the local file survive, which is why this uses `toml_edit` rather than a
/// value round-trip: a user's annotated config must not come back stripped.
///
/// Returns `Err` if either side fails to parse, and the caller must then leave
/// the local file untouched. A malformed upstream is never a reason to rewrite
/// a working local file.
pub fn merge_additive(local: &str, upstream: &str) -> Result<(String, MergeReport), String> {
    merge_additive_with_deny(local, upstream, &[])
}

/// Merge `upstream` into `local`, adding only keys `local` lacks, except
/// dotted paths in `deny` and everything beneath them.
///
/// `deny` exists because the additive rule is *too* faithful for keys the
/// user deliberately removed. A section deleted from a config file — a
/// retired provider, an unused profile — looks exactly like one the user
/// never had, so the next sync puts it back. `rsi/pruned.toml` records those
/// dotted paths per file and hands them here so the deletion sticks.
///
/// A denied path is skipped whole: its subtree is never visited, so denying
/// a table also denies every key inside it. Denying a leaf skips only that
/// key. The list is rooted at the document, e.g. `providers.zai`.
pub fn merge_additive_with_deny(
    local: &str,
    upstream: &str,
    deny: &[String],
) -> Result<(String, MergeReport), String> {
    let mut local_doc: DocumentMut = local
        .parse()
        .map_err(|e| format!("local file is not valid TOML: {e}"))?;
    let upstream_doc: DocumentMut = upstream
        .parse()
        .map_err(|e| format!("upstream template is not valid TOML: {e}"))?;

    let denied: HashSet<&str> = deny.iter().map(String::as_str).collect();
    let mut report = MergeReport::default();
    merge_table(
        local_doc.as_table_mut(),
        upstream_doc.as_table(),
        "",
        &denied,
        &mut report,
    );
    Ok((local_doc.to_string(), report))
}

/// Recursively add missing keys from `up` into `loc`.
fn merge_table(
    loc: &mut Table,
    up: &Table,
    prefix: &str,
    deny: &HashSet<&str>,
    report: &mut MergeReport,
) {
    for (key, up_item) in up.iter() {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };

        // A path the user deleted stays deleted: skip it and its whole
        // subtree, so a retired `[providers.zai]` is not resurrected.
        if deny.contains(path.as_str()) {
            continue;
        }

        match loc.get_mut(key) {
            // Present on both sides and both are tables: recurse, so a new
            // model inside an existing provider is still delivered.
            Some(Item::Table(loc_sub)) => {
                if let Item::Table(up_sub) = up_item {
                    merge_table(loc_sub, up_sub, &path, deny, report);
                }
            }
            // Present as a value. Left alone on purpose: this is the user's
            // setting, and upstream has no business overwriting it.
            Some(_) => {}
            // Absent: this is what upstream is for. A brand-new subtree is
            // cloned whole, but denied descendants are pruned out of the
            // clone first: a parent the local file never had must not smuggle
            // a denied child back in with it.
            None => {
                let mut item = up_item.clone();
                prune_denied(&mut item, &path, deny);
                if is_empty_table(&item) {
                    continue;
                }
                loc.insert(key, item);
                report.added.push(path);
            }
        }
    }
}

/// Remove denied dotted paths from an upstream item before it is inserted.
///
/// The top-of-loop check only fires for keys the local file already has a
/// parent for. When the whole subtree is new, `merge_table` never descends
/// into it, so the deny list has to be applied to the clone directly.
fn prune_denied(item: &mut Item, prefix: &str, deny: &HashSet<&str>) {
    let Item::Table(table) = item else {
        return;
    };
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if deny.contains(path.as_str()) {
            table.remove(&key);
        } else if let Some(child) = table.get_mut(&key) {
            prune_denied(child, &path, deny);
        }
    }
}

/// True when the item is a table that ended up with no keys, i.e. every
/// key it carried was denied.
fn is_empty_table(item: &Item) -> bool {
    matches!(item, Item::Table(t) if t.is_empty())
}
