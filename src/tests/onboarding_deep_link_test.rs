//! Setup command resolution: the direct commands (#1981), `/doctor` and
//! `/models` (#1665), bare `/onboard`, and the legacy `/onboard:<sub>` fallback.
//!
//! `/doctor` used to rewrite itself to the suffix `health` and ride the same
//! dispatch arm a typed `/onboard:health` rode, keeping alive a spelling that
//! was retired when `/doctor` became the standalone health checker. Two
//! entry points to one step, one of them undocumented and unautocompleted,
//! is the one that rots.

use crate::tui::onboarding::OnboardingStep;
use crate::tui::onboarding::deep_link::{
    DeepLink, LEGACY_ONBOARD_SUBCOMMANDS, SETUP_COMMANDS, is_setup_command, resolve,
};

#[test]
fn doctor_opens_the_health_check() {
    let (link, arg) = resolve("/doctor", "/doctor");
    assert_eq!(link, DeepLink::Step(OnboardingStep::HealthCheck));
    assert_eq!(arg, "");
}

#[test]
fn onboard_health_no_longer_resolves_to_a_step() {
    let (link, _) = resolve("/onboard:health", "/onboard:health");
    assert_eq!(
        link,
        DeepLink::Unknown("health".to_string()),
        "/doctor is the health checker; the /onboard:health spelling was retired with it"
    );
}

#[test]
fn health_is_absent_from_the_subcommand_table() {
    assert!(
        !LEGACY_ONBOARD_SUBCOMMANDS
            .iter()
            .any(|(name, _)| *name == "health"),
        "a health subcommand would reintroduce the second path into HealthCheck"
    );
}

#[test]
fn bare_onboard_runs_the_full_wizard() {
    let (link, arg) = resolve("/onboard", "/onboard");
    assert_eq!(link, DeepLink::FullWizard);
    assert_eq!(arg, "");
}

#[test]
fn models_is_the_provider_step() {
    let (link, _) = resolve("/models", "/models");
    assert_eq!(link, DeepLink::Step(OnboardingStep::ProviderAuth));
}

#[test]
fn every_legacy_spelling_still_resolves_to_its_step() {
    // Back-compat fallback (#1981): brain files and habits still say these.
    for (name, step) in LEGACY_ONBOARD_SUBCOMMANDS {
        let input = format!("/onboard:{name}");
        let (link, _) = resolve(&input, &input);
        assert_eq!(
            link,
            DeepLink::Step(*step),
            "/onboard:{name} must open {step:?}"
        );
    }
}

#[test]
fn channel_argument_survives_resolution() {
    // #271: the argument is read off the full input, never off the first word.
    let (link, arg) = resolve("/onboard:channels", "/onboard:channels whatsapp");
    assert_eq!(link, DeepLink::Step(OnboardingStep::Channels));
    assert_eq!(arg, "whatsapp");
}

#[test]
fn unknown_suffix_is_reported_as_unknown_not_silently_accepted() {
    let (link, _) = resolve("/onboard:gateway", "/onboard:gateway");
    assert_eq!(link, DeepLink::Unknown("gateway".to_string()));
}

#[test]
fn every_direct_command_resolves_to_its_step() {
    for (name, step) in SETUP_COMMANDS {
        assert!(is_setup_command(name));
        let (link, arg) = resolve(name, name);
        assert_eq!(link, DeepLink::Step(*step), "{name} must open {step:?}");
        assert_eq!(arg, "");
    }
}

#[test]
fn each_legacy_spelling_opens_the_same_step_as_its_direct_command() {
    for (legacy, step) in LEGACY_ONBOARD_SUBCOMMANDS {
        assert!(
            SETUP_COMMANDS.iter().any(|(_, s)| s == step),
            "/onboard:{legacy} opens {step:?}, which no direct command reaches"
        );
    }
}

#[test]
fn direct_channels_command_keeps_its_argument() {
    let (link, arg) = resolve("/channels", "/channels telegram");
    assert_eq!(link, DeepLink::Step(OnboardingStep::Channels));
    assert_eq!(arg, "telegram");
}

#[test]
fn only_the_channel_step_takes_an_argument() {
    let (_, arg) = resolve("/voice", "/voice whatever");
    assert_eq!(arg, "");
}

#[test]
fn onboard_prefixed_words_are_not_direct_commands() {
    assert!(!is_setup_command("/onboard"));
    assert!(!is_setup_command("/onboard:voice"));
}
