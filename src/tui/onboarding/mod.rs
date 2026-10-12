//! Onboarding Wizard
//!
//! A TUI-based onboarding wizard for first-time OpenCrabs users.
//! Handles mode selection, provider/auth setup, workspace, channels,
//! voice, image, daemon installation, health check and brain setup.

mod brain;
mod channels;
mod config;
pub mod deep_link;
mod fetch;
pub(crate) mod helpers;
mod input;
pub(crate) mod key_field;
mod keys;
mod models;
mod navigation;
pub mod state;
mod types;
pub mod voice;
pub(crate) mod voice_chain;
mod wizard;

// Re-export all public types
pub use types::{
    AuthField, BrainField, CHANNEL_NAMES, ChannelTestStatus, CodexDeviceFlowStatus, DiscordField,
    EXISTING_KEY_SENTINEL, GitHubDeviceFlowStatus, HealthStatus, ImageField, OnboardingStep,
    PROVIDERS, ProviderInfo, SlackField, SttProvider, TEMPLATE_FILES, TelegramField, TrelloField,
    TtsProvider, VoiceField, WhatsAppField, WizardAction, WizardMode,
};

pub use wizard::OnboardingWizard;

pub(crate) use brain::parse_brain_sections;
#[cfg(test)]
pub(crate) use fetch::opencode_models_url;
pub use fetch::{fetch_provider_models, is_first_time};
// merge_minimax_baseline is used by tests via `crate::tui::onboarding::fetch::...`;
// the module is private but the function is `pub(crate)`, accessible only when
// the `fetch` module is reachable. Test access goes through a cfg(test) bridge.
#[cfg(test)]
pub(crate) use fetch::{merge_minimax_baseline, xiaomi_baseline_models};
// build_onboarding_systemd_unit is used by tests via
// `crate::tui::onboarding::build_onboarding_systemd_unit`; `config` is private,
// so it needs the same cfg(test) bridge as `fetch` above. The path is `self::`
// because `config` is also an external crate dependency, which would make a
// bare `use config::…` ambiguous.
#[cfg(test)]
pub(crate) use self::config::build_onboarding_systemd_unit;

/// System prompt sent once after the user completes first-time onboarding.
/// Starts with `[SYSTEM:` so it's hidden from the user display but processed
/// by the agent. The agent uses its personality (SOUL.md) and user knowledge
/// (USER.md) to craft a personalized surprise instead of a hardcoded greeting.
pub const WELCOME_MESSAGE: &str = "[SYSTEM: Onboarding complete. Check your environment — use load_config to see what's configured and tool_search to discover what you have. You know who your user is (USER.md) and you have your personality (SOUL.md). Now do something that'll make them go 'holy shit'. Not a boring greeting. Not 'hey how can I help'. Something only YOU would say, like your best friend just walked in. Be creative, be unexpected. Then ask if they want daily check-ins, task reminders, or heartbeat — you can set those up with cron_manage right now.]";

/// Hidden system prompt for a quick start. Quick start skips the profile and
/// soul steps, so USER.md is still empty. The agent introduces itself and
/// interviews the user to fill it in, one or two questions at a time.
/// SOUL.md stays as shipped unless the user asks to change it.
pub const WELCOME_QUICK_START_MESSAGE: &str = "[SYSTEM: Setup just finished on the OpenCrabs provider, model deepseek-v4.1-flash. Quick start kept the defaults: workspace ~/.opencrabs, SOUL.md as seeded, daemon off. You know nothing about the user yet. Introduce yourself in one or two short lines, in your own voice from SOUL.md. Then get to know them: ask their name, what they do, what they want help with, and how they like to be addressed. Ask one or two questions at a time, never a form. As they answer, save each fact to USER.md with write_opencrabs_file using operation=append. Keep entries short and factual. Do not rewrite SOUL.md unless they explicitly ask you to change your personality. Once you know who they are, offer, without setting anything up, the next options: a fallback provider, speech to text and text to speech, vision, and image generation. Only configure one if they say yes.]";

/// Pick the hidden welcome prompt for the flow the user just finished.
pub fn welcome_message_for(mode: WizardMode) -> &'static str {
    match mode {
        WizardMode::QuickStart => WELCOME_QUICK_START_MESSAGE,
        WizardMode::Advanced => WELCOME_MESSAGE,
    }
}
