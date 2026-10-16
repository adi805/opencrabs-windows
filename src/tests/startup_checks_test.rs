//! Tests for the startup config diagnostics (#477): ignored
//! working_directory keys are flagged (they are not in the schema, serde
//! drops them silently), and default_provider without force_default gets
//! the escape-hatch hint.

use crate::config::startup_checks::startup_warnings;
use crate::config::{
    ChannelsConfig, Config, GoogleChatConfig, IMessageConfig, ProviderConfig, ProviderConfigs,
    TelegramConfig, WhatsAppConfig,
};

fn config_with_default(dp: Option<&str>, force: bool) -> Config {
    Config {
        providers: ProviderConfigs {
            minimax: Some(ProviderConfig {
                enabled: true,
                api_key: Some("key".into()),
                default_model: Some("MiniMax-M3".into()),
                force_default: force,
                ..Default::default()
            }),
            ..Default::default()
        },
        agent: crate::config::AgentConfig {
            default_provider: dp.map(str::to_string),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn clean_config_produces_no_warnings() {
    let warns = startup_warnings(&config_with_default(None, false), Some("[agent]\n"));
    assert!(warns.is_empty(), "{warns:?}");
}

#[test]
fn ignored_working_directory_key_is_flagged() {
    let raw = "[agent]\nworking_directory = \"/nonexistent/void\"\n";
    let warns = startup_warnings(&config_with_default(None, false), Some(raw));
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("has NO effect"));
    assert!(warns[0].contains("does not exist on disk"));
}

#[test]
fn existing_dir_still_flags_the_ignored_key_without_missing_note() {
    let raw = "working_directory = \"/tmp\"\n";
    let warns = startup_warnings(&config_with_default(None, false), Some(raw));
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("has NO effect"));
    assert!(!warns[0].contains("does not exist on disk"));
}

#[test]
fn default_provider_without_force_default_hints_the_flag() {
    let warns = startup_warnings(&config_with_default(Some("minimax"), false), None);
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("NEW sessions only"));
    assert!(warns[0].contains("force_default = true"));
    assert!(warns[0].contains("[providers.minimax]"));
}

#[test]
fn default_provider_with_force_default_stays_quiet() {
    let warns = startup_warnings(&config_with_default(Some("minimax"), true), None);
    assert!(warns.is_empty(), "{warns:?}");
}

#[test]
fn unknown_default_provider_is_flagged() {
    let warns = startup_warnings(&config_with_default(Some("nonexistent"), false), None);
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("does not match any configured provider section"));
}

// ── fallback-chain entries that name nothing ──────────────────────────

/// A config whose custom providers are `defined`, with `chain` as the
/// fallback provider list.
fn config_with_chain(defined: &[&str], chain: &[&str]) -> Config {
    use std::collections::BTreeMap;
    let mut custom = BTreeMap::new();
    for name in defined {
        custom.insert(
            (*name).to_string(),
            ProviderConfig {
                enabled: true,
                api_key: Some("key".into()),
                base_url: Some("https://example.invalid/v1".into()),
                ..Default::default()
            },
        );
    }
    Config {
        providers: ProviderConfigs {
            custom: Some(custom),
            fallback: Some(crate::config::types::FallbackProviderConfig {
                generation: vec![],
                enabled: true,
                providers: chain.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn a_chain_entry_naming_nothing_is_reported() {
    // The chain skips it and the next provider answers, so without this the
    // user sees a model they never picked and no reason for it.
    let cfg = config_with_chain(&["modelscope-qwen37-max"], &["modelscope-qwen37max"]);
    let warns = startup_warnings(&cfg, None);
    assert!(
        warns
            .iter()
            .any(|w| w.contains("modelscope-qwen37max") && w.contains("skipped silently")),
        "expected the dangling entry to be reported, got {warns:?}"
    );
}

#[test]
fn a_separator_slip_names_the_provider_that_was_meant() {
    // One missing hyphen was the real case; the letters and digits match
    // exactly, so naming the intended provider costs no guesswork.
    let cfg = config_with_chain(&["modelscope-qwen37-max"], &["modelscope-qwen37max"]);
    let warns = startup_warnings(&cfg, None);
    assert!(
        warns
            .iter()
            .any(|w| w.contains("did you mean \"modelscope-qwen37-max\"")),
        "expected the near match to be suggested, got {warns:?}"
    );
}

#[test]
fn a_chain_of_configured_providers_is_quiet() {
    let cfg = config_with_chain(&["alpha", "beta"], &["alpha", "beta"]);
    let warns = startup_warnings(&cfg, None);
    assert!(
        !warns.iter().any(|w| w.contains("providers.fallback")),
        "a correct chain must not warn, got {warns:?}"
    );
}

#[test]
fn an_unrelated_name_is_reported_without_a_guess() {
    // No near match exists, so the warning must not invent one.
    let cfg = config_with_chain(&["alpha"], &["something-else-entirely"]);
    let warns = startup_warnings(&cfg, None);
    let hit = warns
        .iter()
        .find(|w| w.contains("something-else-entirely"))
        .expect("the dangling entry must still be reported");
    assert!(!hit.contains("did you mean"), "must not guess, got {hit}");
}

/// A channel section that is enabled but has no implementation must say so.
///
/// `GoogleChatConfig` and `IMessageConfig` live in the schema and are
/// documented as placeholders, but no module reads them, so `enabled = true`
/// is a silent no-op. Turning a channel on and seeing no traffic is
/// indistinguishable from a bad token, which is the archaeology this
/// function exists to end.
#[test]
fn an_enabled_unimplemented_channel_is_reported() {
    let cfg = Config {
        channels: ChannelsConfig {
            google_chat: GoogleChatConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let warns = startup_warnings(&cfg, None);
    let hit = warns
        .iter()
        .find(|w| w.contains("[channels.google_chat]"))
        .unwrap_or_else(|| panic!("an enabled placeholder channel must warn, got {warns:?}"));
    assert!(
        hit.contains("NO effect"),
        "the warning must state the effect is nil, got {hit}"
    );
}

#[test]
fn a_disabled_unimplemented_channel_is_not_reported() {
    let cfg = Config {
        channels: ChannelsConfig {
            google_chat: GoogleChatConfig::default(),
            imessage: IMessageConfig::default(),
            ..Default::default()
        },
        ..Default::default()
    };
    let warns = startup_warnings(&cfg, None);
    assert!(
        !warns
            .iter()
            .any(|w| w.contains("google_chat") || w.contains("imessage")),
        "a channel left disabled must stay quiet, got {warns:?}"
    );
}

// ── Ownership decided by list position (F-CFG-02) ───────────────────────────
//
// `config::owner::is_owner` falls back to the FIRST allow-list entry when
// `bot_owner` is unset, and `seed_bot_owner` writes that same entry into
// `bot_owner`. Order is therefore a privilege grant, and it was silent.

fn config_with_channels(channels: ChannelsConfig) -> Config {
    Config {
        channels,
        ..Default::default()
    }
}

#[test]
fn an_owner_decided_by_position_is_reported() {
    let cfg = config_with_channels(ChannelsConfig {
        telegram: TelegramConfig {
            allowed_users: vec!["111".into(), "222".into()],
            ..Default::default()
        },
        ..Default::default()
    });
    let warns = startup_warnings(&cfg, None);
    let hit = warns
        .iter()
        .find(|w| w.contains("[channels.telegram]"))
        .unwrap_or_else(|| panic!("positional ownership must warn, got {warns:?}"));
    assert!(
        hit.contains("FIRST") && hit.contains("bot_owner"),
        "the warning must name both the rule and the key that pins it, got {hit}"
    );
}

#[test]
fn an_explicit_owner_is_quiet() {
    let cfg = config_with_channels(ChannelsConfig {
        telegram: TelegramConfig {
            allowed_users: vec!["111".into(), "222".into()],
            bot_owner: vec!["222".into()],
            ..Default::default()
        },
        ..Default::default()
    });
    let warns = startup_warnings(&cfg, None);
    assert!(
        !warns.iter().any(|w| w.contains("[channels.telegram]")),
        "an explicit bot_owner makes position irrelevant, got {warns:?}"
    );
}

#[test]
fn a_channel_with_no_allow_list_is_quiet() {
    // No allow list means no position to grant ownership from: `is_owner`
    // returns false, so there is nothing to warn about.
    let cfg = config_with_channels(ChannelsConfig {
        telegram: TelegramConfig::default(),
        whatsapp: WhatsAppConfig::default(),
        ..Default::default()
    });
    let warns = startup_warnings(&cfg, None);
    assert!(
        !warns
            .iter()
            .any(|w| w.contains("[channels.telegram]") || w.contains("[channels.whatsapp]")),
        "an unconfigured channel must stay quiet, got {warns:?}"
    );
}

#[test]
fn the_phone_channel_is_covered_too() {
    // WhatsApp gates on `allowed_phones`, not `allowed_users`; the check has to
    // read the right field or it silently covers four channels and not five.
    let cfg = config_with_channels(ChannelsConfig {
        whatsapp: WhatsAppConfig {
            allowed_phones: vec!["+62812".into()],
            ..Default::default()
        },
        ..Default::default()
    });
    let warns = startup_warnings(&cfg, None);
    assert!(
        warns.iter().any(|w| w.contains("[channels.whatsapp]")),
        "WhatsApp ownership by position must warn, got {warns:?}"
    );
}
