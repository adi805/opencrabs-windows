//! Resolve a setup command to the wizard step it opens: the direct commands
//! (`/models`, `/workspace`, `/channels`, `/voice`, `/image`, `/daemon`,
//! `/brain`, `/doctor`), bare `/onboard` for the full wizard, and the legacy
//! `/onboard:<step>` spellings kept as a silent fallback (#1981).
//!
//! Pulled out of `handle_slash_command` so the command set has one home.
//! Dispatch reads [`SETUP_COMMANDS`], and so do the witness tests that hold
//! the slash registry and the README table to the same list. A doc row or a
//! registry entry that outlives its arm (#1664) then fails the build instead
//! of the user.

use super::OnboardingStep;

/// Every direct setup command, with the step it opens (#1981). These are what
/// the `/` autocomplete, the help dialog and the README offer. `/models` and
/// `/doctor` predate the rest; the others replaced `/onboard:<step>`.
pub const SETUP_COMMANDS: &[(&str, OnboardingStep)] = &[
    ("/models", OnboardingStep::ProviderAuth),
    ("/workspace", OnboardingStep::Workspace),
    ("/channels", OnboardingStep::Channels),
    ("/voice", OnboardingStep::VoiceSetup),
    ("/image", OnboardingStep::ImageSetup),
    ("/daemon", OnboardingStep::Daemon),
    ("/brain", OnboardingStep::BrainSetup),
    ("/doctor", OnboardingStep::HealthCheck),
];

/// The retired `/onboard:<name>` spellings, still resolved so brain files and
/// habits that say them keep working (#1981), but offered on no surface.
///
/// `health` is deliberately absent: `/doctor` is the health checker and the
/// `/onboard:health` spelling was retired with it (#1665).
pub const LEGACY_ONBOARD_SUBCOMMANDS: &[(&str, OnboardingStep)] = &[
    ("provider", OnboardingStep::ProviderAuth),
    ("workspace", OnboardingStep::Workspace),
    ("channels", OnboardingStep::Channels),
    ("voice", OnboardingStep::VoiceSetup),
    ("image", OnboardingStep::ImageSetup),
    ("daemon", OnboardingStep::Daemon),
    ("brain", OnboardingStep::BrainSetup),
];

/// What a slash input resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    /// Bare `/onboard`: the full wizard, starting at the mode selector.
    FullWizard,
    /// A recognised setup command, locked to a single step.
    Step(OnboardingStep),
    /// `/onboard:<something>` with no arm behind it. Carries the suffix so the
    /// caller can name it back to the user instead of silently guessing.
    Unknown(String),
}

/// Whether `command` (the first word) is one of the direct setup commands.
pub fn is_setup_command(command: &str) -> bool {
    SETUP_COMMANDS.iter().any(|(name, _)| *name == command)
}

/// Resolve `command` (the first word) plus the full `input` line.
///
/// Returns the link and the trailing argument, if any. The argument is what
/// makes `/channels whatsapp` land on the WhatsApp dialog rather than the
/// channel menu (#271), so it is read off the full input, never off the
/// first word. Only the channel step takes one.
pub fn resolve<'a>(command: &str, input: &'a str) -> (DeepLink, &'a str) {
    if let Some((_, step)) = SETUP_COMMANDS.iter().find(|(name, _)| *name == command) {
        let arg = if *step == OnboardingStep::Channels {
            input.split_whitespace().nth(1).unwrap_or("")
        } else {
            ""
        };
        return (DeepLink::Step(*step), arg);
    }

    let suffix: &'a str = input
        .strip_prefix("/onboard")
        .unwrap_or("")
        .trim_start_matches(':');
    let mut parts = suffix.split_whitespace();
    let head = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("");
    if head.is_empty() {
        return (DeepLink::FullWizard, "");
    }
    match LEGACY_ONBOARD_SUBCOMMANDS
        .iter()
        .find(|(name, _)| *name == head)
    {
        Some((_, step)) => (DeepLink::Step(*step), arg),
        None => (DeepLink::Unknown(head.to_string()), ""),
    }
}

/// The message shown for an unrecognised `/onboard:` suffix.
///
/// Naming the valid set beats opening the full wizard, which made a typo or
/// a stale documented name indistinguishable from bare `/onboard` (#1664).
/// It names the direct commands: the legacy spellings are a fallback, not
/// something to steer anyone toward (#1981).
pub fn unknown_suffix_message(suffix: &str) -> String {
    let valid = SETUP_COMMANDS
        .iter()
        .map(|(name, _)| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "⚠️ Unknown setup step `{suffix}`. Setup commands: {valid}. \
         Bare `/onboard` runs the full wizard."
    )
}
