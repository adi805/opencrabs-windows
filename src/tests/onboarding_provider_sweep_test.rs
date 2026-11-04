//! Provider enablement sweep regressions (5 Nov 2026).
//!
//! `apply_config` opens by writing `enabled = false` to every section in
//! `[providers.*]` and only then enables the wizard's pick. That is the user's
//! own choice when they actually made one, and a wipe when they did not: the
//! saves that run on the way OUT of the wizard (Esc / Ctrl+C, going back a
//! step, the pre-generation save) ran it with whatever the wizard happened to
//! be showing. `config.toml` on this machine lost
//! `providers.opencrabs.enabled` and `providers.custom.ocdev.enabled` that way,
//! and the next boot fell through to a custom provider nobody had chosen.
//!
//! These lock the split shut: a deliberate pick still sweeps, an incidental
//! save does not.

use crate::config::profile::with_home_override;
use crate::tui::onboarding::{OnboardingStep, OnboardingWizard};

/// Anthropic: a built-in section that is not the custom one under test. Set
/// explicitly because `OnboardingWizard::new()` auto-detects an enabled custom
/// provider and would select `ocdev` itself, which would make the sweep test
/// assert the opposite of its own premise.
const PICK: usize = 0;

fn temp_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&home).expect("create .opencrabs");
    (dir, home)
}

fn flag(home: &std::path::Path, segments: &[&str], key: &str) -> Option<bool> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = raw.parse().expect("valid TOML");
    // Walk as &dyn TableLike so each nested get() keeps the same type;
    // as_table_like() returns None on absence instead of panicking.
    let mut table: &dyn toml_edit::TableLike = doc.as_table();
    for seg in segments {
        table = table.get(seg)?.as_table_like()?;
    }
    match table.get(key)? {
        toml_edit::Item::Value(v) => v.as_bool().or_else(|| v.as_str().map(|s| s == "true")),
        _ => None,
    }
}

/// Config with two providers already in use, the shape this install had.
fn seed_two_enabled_providers() {
    crate::config::Config::write_key("providers.opencrabs", "enabled", "true")
        .expect("built-in on");
    crate::config::Config::write_key("providers.custom.ocdev", "enabled", "true")
        .expect("custom on");
    crate::config::Config::write_key(
        "providers.custom.ocdev",
        "base_url",
        "https://example.invalid/v1",
    )
    .expect("custom base_url");
    // cleanup_empty_custom_providers drops a custom section with neither URL nor
    // model, so keep both: the sweep assertion is about the flag, not survival.
    crate::config::Config::write_key("providers.custom.ocdev", "default_model", "m")
        .expect("custom model");
}

/// The wizard parked on the provider step of a quick jump, which is the state
/// the destructive save ran from.
fn wizard_on_provider_step() -> OnboardingWizard {
    let mut wizard = OnboardingWizard {
        quick_jump: true,
        step: OnboardingStep::ProviderAuth,
        ..OnboardingWizard::default()
    };
    wizard.ps.selected_provider = PICK;
    wizard.ps.custom_name.clear();
    wizard
}

#[test]
fn incidental_save_keeps_other_providers_enabled() {
    let (_temp, home) = temp_home();
    with_home_override(home.clone(), || {
        seed_two_enabled_providers();

        // What quit / cancel / pre-generation save now call.
        wizard_on_provider_step()
            .apply_config_preserving_providers()
            .expect("save succeeds");

        assert_eq!(
            flag(&home, &["providers", "opencrabs"], "enabled"),
            Some(true),
            "a save the user never asked for must not turn the built-in provider off"
        );
        assert_eq!(
            flag(&home, &["providers", "custom", "ocdev"], "enabled"),
            Some(true),
            "a provider other sessions are pinned to must survive a cancel"
        );
    });
}

#[test]
fn deliberate_pick_still_sweeps_the_others() {
    let (_temp, home) = temp_home();
    with_home_override(home.clone(), || {
        seed_two_enabled_providers();

        // What a completed pick / QuickJumpDone still call.
        wizard_on_provider_step()
            .apply_config()
            .expect("save succeeds");

        assert_eq!(
            flag(&home, &["providers", "custom", "ocdev"], "enabled"),
            Some(false),
            "choosing a provider still sweeps, so the pick becomes the active one"
        );
        assert_eq!(
            flag(&home, &["providers", "anthropic"], "enabled"),
            Some(true),
            "the pick itself is enabled by the same save"
        );
    });
}
