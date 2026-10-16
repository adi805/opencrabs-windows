//! One database, two timestamp conventions, and a gate so a third cannot
//! arrive by accident (F-DB-06).
//!
//! A timestamp column is EITHER an integer holding Unix epoch seconds, read with
//! `timestamp_col` / `opt_timestamp_col`, OR RFC-3339 TEXT, read with
//! `rfc3339_col` / `opt_rfc3339_col`. Both work, and both are in use:
//! `cron_jobs`, `cron_job_runs`, `feedback_ledger` and `goal_state` store TEXT,
//! everything else stores an integer.
//!
//! The hazard is a comparison across the two. `created_at >= ?1` where one side is
//! a number and the other a string is not an error in SQLite: the comparison
//! falls back to type order, so an integer sorts before every string and the
//! predicate answers about the type rather than about time. Silently.
//!
//! Nothing mixed the two when this test was written, so it is a gate rather than
//! a fix: the migrations are the source of truth for a column's type, and these
//! assertions read them. A new table has to choose, and choosing TEXT puts it in
//! the list below, which is a deliberate act rather than an accident.
//!
//! The scan reads `src/migrations/*.sql` at run time, so a migration added
//! tomorrow is covered without touching this file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The tables that store their timestamps as RFC-3339 TEXT.
///
/// Three declare the form as their column DEFAULT, so their rows are written by
/// SQL and the timestamp never passes through Rust. `goal_state` has no DEFAULT
/// and binds `Utc::now().to_rfc3339()` from Rust, having followed the three
/// rather than introducing the convention.
const RFC3339_TIMESTAMP_TABLES: &[&str] = &[
    "cron_jobs",
    "cron_job_runs",
    "feedback_ledger",
    "goal_state",
];

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/migrations")
}

fn migration_sources() -> Vec<(String, String)> {
    let dir = migrations_dir();
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        out.push((name, body));
    }
    out.sort();
    assert!(
        out.len() > 50,
        "only {} migration files found; the scan is reading the wrong directory",
        out.len()
    );
    out
}

/// Whether a column name looks like a timestamp.
///
/// Narrow on purpose: `status`, `path` and `metadata` are TEXT columns in these
/// tables and have nothing to do with the convention, so a scan that took every
/// TEXT column would report the whole schema.
fn is_timestamp_column(col: &str) -> bool {
    // `_accessed` is here because `recent_paths.last_accessed` is an INTEGER
    // epoch like every `_at` column, and leaving it out made the scan skip the
    // table entirely: the gate reported a clean schema while never looking at
    // one of its timestamp columns.
    col.ends_with("_at")
        || col.ends_with("_time")
        || col.ends_with("_date")
        || col.ends_with("_accessed")
}

/// The declared type of each timestamp column, per table, across every
/// `CREATE TABLE` and `ALTER TABLE … ADD COLUMN` in the migrations.
fn timestamp_columns_by_table() -> BTreeMap<String, BTreeMap<String, String>> {
    let mut tables: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (file, body) in migration_sources() {
        collect_timestamp_columns(&file, &body, &mut tables);
    }
    tables
}

/// Add every timestamp column declared by one migration to `tables`.
///
/// Takes the file name and its body rather than reading them so the parser can be
/// exercised on synthetic SQL: the two declaration forms it has to handle
/// (`CREATE TABLE`, `ALTER TABLE … ADD COLUMN`) both appear in the real migrations,
/// but a parser test that can only run against them cannot test a shape the tree
/// does not happen to contain.
fn collect_timestamp_columns(
    file: &str,
    body: &str,
    tables: &mut BTreeMap<String, BTreeMap<String, String>>,
) {
    // CREATE TABLE [IF NOT EXISTS] name ( … );
    for (name, block) in create_table_blocks(body) {
        // Insert only when a timestamp column is actually found. Creating the
        // entry up front put every table into the map, so a table with no
        // timestamp column arrived with an empty set and tripped the
        // "one convention per table" assertion below instead of being skipped.
        for (col, ty) in column_declarations(&block) {
            if is_timestamp_column(&col) {
                tables.entry(name.clone()).or_default().insert(col, ty);
            }
        }
    }

    // ALTER TABLE name ADD COLUMN col TYPE …;
    for line in body.lines() {
        let line = line.trim();
        let upper = line.to_ascii_uppercase();
        if !upper.starts_with("ALTER TABLE") {
            continue;
        }
        let Some(add_at) = upper.find("ADD COLUMN") else {
            continue;
        };
        let rest = &line[add_at + "ADD COLUMN".len()..];
        let mut parts = rest.split_whitespace();
        let Some(col) = parts.next() else { continue };
        let Some(ty) = parts.next() else { continue };
        let col = col.to_ascii_lowercase();
        if !is_timestamp_column(&col) {
            continue;
        }
        // The table name sits between ALTER TABLE and ADD COLUMN.
        let table = upper["ALTER TABLE".len()..add_at]
            .trim()
            .trim_start_matches("IF EXISTS ")
            .trim()
            .to_ascii_lowercase();
        if table.is_empty() {
            panic!("{file}: ALTER TABLE with no table name: {line}");
        }
        let ty = ty.trim_end_matches([',', ';']).to_ascii_uppercase();
        tables.entry(table).or_default().insert(col, ty);
    }
}

/// `(table name, body)` for every `CREATE TABLE` in `sql`, case-insensitive on
/// the keyword and tolerant of `IF NOT EXISTS`.
fn create_table_blocks(sql: &str) -> Vec<(String, String)> {
    let upper = sql.to_ascii_uppercase();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = upper[from..].find("CREATE TABLE") {
        let at = from + rel;
        let after = &sql[at + "CREATE TABLE".len()..];
        let after_upper = &upper[at + "CREATE TABLE".len()..];
        let skip = if after_upper.trim_start().starts_with("IF NOT EXISTS") {
            "IF NOT EXISTS".len()
        } else {
            0
        };
        let rest = after.trim_start()[skip..].trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // The body runs from the first '(' to the matching ')'. Column
        // declarations carry no nested parens beyond a DEFAULT expression, so
        // tracking depth is enough and cheaper than a parser.
        let Some(open) = rest.find('(') else { break };
        let mut depth = 0i32;
        let mut end = None;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        if !name.is_empty() {
            out.push((name.to_ascii_lowercase(), rest[open + 1..end].to_string()));
        }
        // Continue past this keyword. The body is not searched for another
        // `CREATE TABLE`, and `end` indexes into `rest` rather than into `sql`,
        // so it cannot be used to advance the outer cursor.
        from = at + "CREATE TABLE".len();
    }
    out
}

/// `(column, declared type)` for each column declaration in a table body.
fn column_declarations(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.split('\n') {
        let line = line.trim().trim_end_matches(',').trim();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        // Skip the table-level constraints, which name no column.
        if [
            "PRIMARY KEY",
            "FOREIGN KEY",
            "UNIQUE",
            "CHECK",
            "CONSTRAINT",
        ]
        .iter()
        .any(|k| upper.starts_with(k))
        {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(col) = parts.next() else { continue };
        let Some(ty) = parts.next() else { continue };
        let col = col.trim_matches('"').to_ascii_lowercase();
        if !col.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        // The type token carries the trailing comma when a comment follows it on
        // the same line (`last_run_at TEXT,  -- when it last ran`), and a
        // semicolon on the last column. Strip both or the type reads as
        // `TEXT,` and matches neither convention.
        let ty = ty.trim_end_matches([',', ';']).to_ascii_uppercase();
        out.push((col, ty));
    }
    out
}

/// Every timestamp column is an integer or TEXT, and every table uses one of the
/// two conventions throughout.
///
/// A table that mixed them would be the hazard in its purest form: two columns
/// from the same row, comparable in SQL, incomparable in fact.
#[test]
fn no_table_mixes_the_two_conventions() {
    let tables = timestamp_columns_by_table();
    assert!(
        tables.len() > 30,
        "the scan found {} tables with a timestamp column, which is too few for this \
         schema; the parser is broken rather than the migrations being small",
        tables.len()
    );

    for (table, cols) in &tables {
        let types: BTreeSet<&str> = cols.values().map(|s| s.as_str()).collect();
        for ty in &types {
            assert!(
                matches!(*ty, "INTEGER" | "TEXT"),
                "{table}: a timestamp column is declared {ty}, which is neither of the two \
                 conventions: {cols:?}"
            );
        }
        assert_eq!(
            types.len(),
            1,
            "{table} mixes the two timestamp conventions across its own columns: {cols:?}"
        );
    }
}

/// The TEXT set is exactly the documented one, so a new table has to choose
/// deliberately: adding a `created_at TEXT` puts it here and fails until the list
/// and the doc on `timestamp_col` name it.
#[test]
fn the_rfc3339_tables_are_exactly_the_documented_set() {
    let tables = timestamp_columns_by_table();
    let found: BTreeSet<String> = tables
        .iter()
        .filter(|(_, cols)| cols.values().any(|t| t == "TEXT"))
        .map(|(name, _)| name.clone())
        .collect();
    let documented: BTreeSet<String> = RFC3339_TIMESTAMP_TABLES
        .iter()
        .map(|s| s.to_string())
        .collect();

    let new: Vec<&String> = found.difference(&documented).collect();
    assert!(
        new.is_empty(),
        "these tables store their timestamps as RFC-3339 TEXT and are not in the documented \
         list (F-DB-06). Add each one to `RFC3339_TIMESTAMP_TABLES` and to the doc on \
         `timestamp_col` in src/db/models.rs, or declare the column INTEGER to match the \
         majority: {new:#?}"
    );

    let stale: Vec<&String> = documented.difference(&found).collect();
    assert!(
        stale.is_empty(),
        "these tables are documented as RFC-3339 but no longer store a TEXT timestamp, so \
         the record has rotted. Remove them from the list: {stale:#?}"
    );
}

/// The two tables the finding names are on the epoch side, and the four documented
/// ones are real. Without this the test above could pass on a schema where the
/// scan silently found nothing.
#[test]
fn the_scan_sees_the_tables_the_finding_named() {
    let tables = timestamp_columns_by_table();
    for epoch in ["sessions", "usage_ledger", "messages", "files"] {
        let cols = tables
            .get(epoch)
            .unwrap_or_else(|| panic!("{epoch} must have a timestamp column"));
        assert!(
            cols.values().all(|t| t == "INTEGER"),
            "{epoch} is on the epoch side of the convention: {cols:?}"
        );
    }
    for text in RFC3339_TIMESTAMP_TABLES {
        assert!(
            tables.contains_key(*text),
            "{text} is documented as an RFC-3339 table but has no timestamp column at all"
        );
    }
}

/// The parser, exercised on the shapes it has to tell apart: `IF NOT EXISTS`, an
/// `ALTER TABLE … ADD COLUMN`, a table-level constraint that names no column, and
/// a body containing a `DEFAULT (…)` expression.
#[test]
fn the_migration_parser_handles_both_declaration_forms() {
    let sql = "CREATE TABLE IF NOT EXISTS demo (\n    id TEXT PRIMARY KEY,\n    \
               created_at INTEGER NOT NULL,\n    PRIMARY KEY (id)\n);\n";
    let blocks = create_table_blocks(sql);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].0, "demo");
    let cols = column_declarations(&blocks[0].1);
    assert!(cols.contains(&("created_at".to_string(), "INTEGER".to_string())));
    assert!(
        !cols.iter().any(|(c, _)| c == "PRIMARY"),
        "a table-level constraint is not a column: {cols:?}"
    );

    // A nested paren inside a DEFAULT must not end the body early.
    let nested = "CREATE TABLE demo2 (\n    created_at TEXT DEFAULT (strftime('%s', 'now')),\n    \
                  updated_at TEXT\n);\n";
    let blocks = create_table_blocks(nested);
    assert_eq!(blocks.len(), 1);
    let cols = column_declarations(&blocks[0].1);
    assert_eq!(cols.len(), 2, "{cols:?}");

    // And the ALTER form reaches the same map.
    let altered = "CREATE TABLE demo3 (id TEXT);\n\
                   ALTER TABLE demo3 ADD COLUMN committed_at INTEGER;\n";
    assert_eq!(create_table_blocks(altered).len(), 1);
    let mut tables: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    collect_timestamp_columns("demo.sql", altered, &mut tables);
    assert_eq!(
        tables.get("demo3").and_then(|c| c.get("committed_at")),
        Some(&"INTEGER".to_string()),
        "an ALTER TABLE ADD COLUMN must land in the same map as a CREATE TABLE: {tables:?}"
    );

    // The trailing separator cases, which is where a naive parse puts `TEXT,`
    // into the map and the type matches neither convention. The real migrations
    // carry both: a comma followed by a comment, and a semicolon on the last
    // column.
    let separators = "CREATE TABLE demo4 (\n    \
                      created_at TEXT, -- when it was created\n    \
                      updated_at INTEGER\n);\n\
                      ALTER TABLE demo4 ADD COLUMN seen_at TEXT;\n";
    let mut tables: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    collect_timestamp_columns("demo.sql", separators, &mut tables);
    assert_eq!(
        tables.get("demo4").and_then(|c| c.get("created_at")),
        Some(&"TEXT".to_string()),
        "a type followed by a comment must not keep the comma: {tables:?}"
    );
    assert_eq!(
        tables.get("demo4").and_then(|c| c.get("seen_at")),
        Some(&"TEXT".to_string()),
        "a type followed by a semicolon must not keep it: {tables:?}"
    );

    assert!(
        is_timestamp_column("committed_at") && !is_timestamp_column("status"),
        "the column-name filter must separate a timestamp from a status"
    );
}
