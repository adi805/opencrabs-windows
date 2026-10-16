//! Migration 2 (`[voice]` to `providers.stt.*` / `providers.tts.*`) must
//! honour a legacy `enabled = true` (#1399).
//!
//! The migration used `entry("enabled").or_insert(true)`, a no-op whenever
//! the provider table already carried `enabled = false`, so a config that
//! said voice was on came out of the migration with voice off and there
//! was no second chance: the `[voice]` table is deleted in the same pass.

use crate::config::Config;
use tempfile::TempDir;

fn migrate(contents: &str) -> (TempDir, toml::Value) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, contents).unwrap();
    Config::migrate_if_needed(&path);
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    (dir, doc)
}

#[test]
fn legacy_tts_enabled_re_enables_a_disabled_openai_table() {
    let (_d, doc) = migrate(
        "[voice]\ntts_enabled = true\ntts_mode = \"api\"\ntts_voice = \"echo\"\n\n\
         [providers.tts.openai]\nenabled = false\n",
    );
    assert_eq!(
        doc["providers"]["tts"]["openai"]["enabled"].as_bool(),
        Some(true)
    );
    assert_eq!(
        doc["providers"]["tts"]["openai"]["voice"].as_str(),
        Some("echo")
    );
    assert!(doc.get("voice").is_none(), "the legacy table is consumed");
}

#[test]
fn legacy_tts_enabled_creates_the_openai_table_when_missing() {
    let (_d, doc) = migrate("[voice]\ntts_enabled = true\ntts_mode = \"api\"\n");
    assert_eq!(
        doc["providers"]["tts"]["openai"]["enabled"].as_bool(),
        Some(true)
    );
}

#[test]
fn legacy_local_stt_enabled_re_enables_a_disabled_local_table() {
    let (_d, doc) = migrate(
        "[voice]\nstt_enabled = true\nstt_mode = \"local\"\nlocal_stt_model = \"local-base\"\n\n\
         [providers.stt.local]\nenabled = false\nmodel = \"local-tiny\"\n",
    );
    assert_eq!(
        doc["providers"]["stt"]["local"]["enabled"].as_bool(),
        Some(true)
    );
    assert_eq!(
        doc["providers"]["stt"]["local"]["model"].as_str(),
        Some("local-tiny"),
        "an existing model choice is kept; only the flag is forced"
    );
}

#[test]
fn legacy_groq_stt_enabled_creates_the_groq_table_when_missing() {
    let (_d, doc) = migrate("[voice]\nstt_enabled = true\nstt_mode = \"api\"\n");
    assert_eq!(
        doc["providers"]["stt"]["groq"]["enabled"].as_bool(),
        Some(true)
    );
}

#[test]
fn legacy_disabled_voice_leaves_provider_flags_alone() {
    let (_d, doc) = migrate(
        "[voice]\ntts_enabled = false\nstt_enabled = false\n\n\
         [providers.tts.openai]\nenabled = false\n",
    );
    assert_eq!(
        doc["providers"]["tts"]["openai"]["enabled"].as_bool(),
        Some(false)
    );
    assert!(doc.get("voice").is_none());
}

// ── a malformed table shape must not panic the loader (F-CFG-02) ─────
//
// `contains_key("providers")` proves presence, not shape. The migration used
// `.as_table_mut().unwrap()`, so a config carrying `providers = 5` next to a
// legacy `[voice]` panicked on startup, and because the loader has no
// `catch_unwind`, `load_last_good_config()` never ran: the agent simply failed
// to start with a snapshot sitting unused beside it.
//
// Every fixture below writes `providers` BEFORE the `[voice]` header. Written
// after it, TOML nests `providers` INSIDE `[voice]`, the top-level lookup finds
// nothing, and the fixture silently stops exercising the path it names while
// the malformed value gets carried off with the legacy table.

#[test]
fn a_non_table_providers_value_does_not_panic_the_voice_migration() {
    let (_d, doc) = migrate("providers = 5\n\n[voice]\nstt_enabled = true\n");
    // The migration is skipped and both sides are left alone: the malformed
    // value is not silently rewritten, and the legacy table is not consumed,
    // so nothing is lost while the config is repaired by hand.
    assert_eq!(doc["providers"].as_integer(), Some(5));
    assert!(
        doc.get("voice").is_some(),
        "a skipped migration must not consume the legacy table"
    );
}

#[test]
fn a_non_table_stt_value_does_not_panic() {
    let (_d, doc) =
        migrate("providers = { stt = 7 }\n\n[voice]\nstt_enabled = true\nstt_mode = \"groq\"\n");
    assert_eq!(doc["providers"]["stt"].as_integer(), Some(7));
    assert!(
        doc.get("voice").is_some(),
        "a skipped STT seed must not consume the legacy table"
    );
}

#[test]
fn a_non_table_tts_value_does_not_panic() {
    let (_d, doc) = migrate(
        "providers = { tts = \"nope\" }\n\n[voice]\ntts_enabled = true\ntts_mode = \"api\"\n",
    );
    assert_eq!(doc["providers"]["tts"].as_str(), Some("nope"));
    assert!(
        doc.get("voice").is_some(),
        "a skipped TTS seed must not consume the legacy table"
    );
}

#[test]
fn a_non_table_engine_value_does_not_panic() {
    let (_d, doc) = migrate(
        "providers = { tts = { openai = 3 } }\n\n[voice]\ntts_enabled = true\ntts_mode = \"api\"\n",
    );
    assert_eq!(doc["providers"]["tts"]["openai"].as_integer(), Some(3));
    assert!(
        doc.get("voice").is_some(),
        "a skipped engine seed must not consume the legacy table"
    );
}

#[test]
fn a_non_table_stt_engine_value_does_not_panic() {
    // The deepest wrong shape: `providers.stt` IS a table, but
    // `providers.stt.groq` is not. The STT seed has nowhere to land, so the
    // whole migration must skip rather than half-apply and delete `[voice]`.
    let (_d, doc) = migrate(
        "providers = { stt = { groq = 3 } }\n\n[voice]\nstt_enabled = true\nstt_mode = \"groq\"\n",
    );
    assert_eq!(doc["providers"]["stt"]["groq"].as_integer(), Some(3));
    assert!(
        doc.get("voice").is_some(),
        "a skipped engine seed must not consume the legacy table"
    );
}

#[test]
fn the_well_formed_case_still_migrates() {
    // The graceful path must not cost the working migration.
    let (_d, doc) = migrate("[voice]\ntts_enabled = true\ntts_mode = \"api\"\ntts_voice = \"x\"\n");
    assert_eq!(
        doc["providers"]["tts"]["openai"]["enabled"].as_bool(),
        Some(true)
    );
    assert!(doc.get("voice").is_none());
}
