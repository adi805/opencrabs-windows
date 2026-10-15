use crate::config::Config;

/// The `/models` picker reads its catalogue from the embedded example, so the
/// built-in provider's model has to be listed there and default to disabled.
#[test]
fn example_config_lists_the_built_in_model_disabled_by_default() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert!(!section.enabled, "built-in provider ships disabled");
    assert_eq!(
        section.default_model.as_deref(),
        Some("deepseek-v4.1-flash")
    );
    assert_eq!(section.models, vec!["deepseek-v4.1-flash".to_string()]);
    // The built-in endpoint is free and fixed at 200k, so the key is not
    // offered in the example and is not honoured if a user adds it (#2066).
    assert_eq!(section.context_window, None);
}

/// Image generation ships on day 0 through the built-in provider, on its own
/// model id (served only on the proxy's image routes, never as chat).
#[test]
fn example_config_sets_the_built_in_image_model() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(
        section.generation_model.as_deref(),
        Some("gemini-3.1-flash-image")
    );
    assert!(
        !section.models.iter().any(|m| m == "gemini-3.1-flash-image"),
        "image model must not appear in the chat model list"
    );
}

/// Voice ships on day 0 the same way: the built-in provider carries the
/// transcription and synthesis model ids, so a fresh install has STT and TTS
/// with nothing to configure. The id is bare (no `alicn/` provider prefix),
/// matching the image model's #2054 treatment.
#[test]
fn example_config_sets_the_built_in_voice_models() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(section.stt_model.as_deref(), Some("qwen3.8-omni-flash"));
    assert_eq!(section.tts_model.as_deref(), Some("qwen3.8-omni-flash"));
    assert!(
        !section.models.iter().any(|m| m == "qwen3.8-omni-flash"),
        "voice model must not appear in the chat model list"
    );
}
