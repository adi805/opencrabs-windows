//! A compaction summary must not report an artifact no tool call produced (#482).
//!
//! Observed 2026-09-21 13:04:26: a background `FullWindow` compaction wrote a
//! 36,373-byte continuation document asserting `PRE-JUDGED VERDICTS (the
//! analytical work is DONE ... transcribe them)`, a 30-row table, a count line
//! quoted as "the exact text, end of part3", and a header quoted as "the exact
//! first two lines of part1". All three `write_file` calls ran at 13:04:28,
//! :39 and :48 — AFTER the summary landed. A summary cannot quote what does
//! not exist.
//!
//! The child agent refused and disagreed with the injected table on 20 of 30
//! rows. That defence rested on a model's judgement; this is the mechanism.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::phantom::{claimed_artifact_unbacked, claimed_artifacts};

/// The incident's own framing, verbatim in shape.
const INCIDENT: &str = "\
# CONTINUATION DOCUMENT

**PRE-JUDGED VERDICTS (the analytical work is DONE — these are the conclusions \
you reached; transcribe them):**

| claim | verdict |
|---|---|
| c1 | HOLDS |

**Count line (exact text, end of part3):** `13 HOLDS · 13 WRONG-MODE · 4 DOES-NOT-HOLD (30 claims)`

**Table header (exact, first two lines of part1):**
";

#[test]
fn the_incident_shape_is_flagged() {
    let claims = claimed_artifacts(INCIDENT);
    assert!(claims.contains(&"part3".to_string()), "got {claims:?}");
    assert!(claims.contains(&"part1".to_string()), "got {claims:?}");
}

#[test]
fn an_artifact_no_call_produced_is_unbacked() {
    // The conversation being summarised, with no write of part1/part3.
    let evidence = "let me read the corpus\nverdict c1 HOLDS\n";
    for claim in claimed_artifacts(INCIDENT) {
        assert!(
            claimed_artifact_unbacked(&claim, evidence),
            "`{claim}` must be unbacked"
        );
    }
}

#[test]
fn a_written_artifact_is_backed() {
    // What a real turn leaves behind: the write tool's own argument.
    let evidence = r#"{"path":"out/part3.md","content":"13 HOLDS ..."}"#;
    assert!(!claimed_artifact_unbacked("part3", evidence));
    assert!(!claimed_artifact_unbacked("out/part3.md", evidence));
}

#[test]
fn prose_about_files_is_not_a_claim() {
    let text = "The corpus lives in part3 of the report and I read it.\n\
                Nothing here is finished.";
    assert!(claimed_artifacts(text).is_empty());
}

#[test]
fn a_quoted_path_is_a_reference_not_a_claim() {
    let text = "The exact text is in `part3.md`, quoted above.";
    assert!(
        claimed_artifacts(text).is_empty(),
        "a backticked path is a reference"
    );
}

#[test]
fn tallies_alone_are_not_artifacts() {
    // The count line is a tally, not an artifact name; `asserted_facts` owns
    // it. This extractor must not double-report it.
    let text = "Count line: `13 HOLDS · 13 WRONG-MODE · 4 DOES-NOT-HOLD (30 claims)`";
    assert!(claimed_artifacts(text).is_empty());
}
