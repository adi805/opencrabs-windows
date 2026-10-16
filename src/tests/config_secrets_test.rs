use crate::config::secrets::*;

#[test]
fn test_secret_string_creation() {
    let secret = SecretString::from_str("my-secret-key");
    assert_eq!(secret.expose_secret(), "my-secret-key");
    assert_eq!(secret.len(), 13);
    assert!(!secret.is_empty());
}

#[test]
fn test_secret_string_debug() {
    let secret = SecretString::from_str("my-secret-key");
    let debug_output = format!("{:?}", secret);
    assert_eq!(debug_output, "[REDACTED]");
    assert!(!debug_output.contains("my-secret-key"));
}

#[test]
fn test_secret_string_display() {
    let secret = SecretString::from_str("my-secret-key");
    let display_output = format!("{}", secret);
    assert_eq!(display_output, "[REDACTED]");
    assert!(!display_output.contains("my-secret-key"));
}

#[test]
fn test_secret_string_from_env_missing() {
    // Test that a non-existent env var returns None (no env loading)
    let result = std::env::var("OPENCRABS_TEST_NONEXISTENT_KEY_12345");
    assert!(result.is_err());
}

/// Serializing is REFUSED, not redacted (F-CFG-01).
///
/// The old behaviour wrote the placeholder `"[REDACTED]"` into the output, which
/// reads as a safety feature and behaves as a wipe: the value is an ordinary
/// string, so a struct holding a secret serialized cleanly, the file lost its key,
/// and the next load read the placeholder back as a literal. Nothing errored, and
/// the only symptom was a provider that stopped authenticating.
#[test]
fn test_secret_string_serialize_is_refused() {
    let secret = SecretString::from_str("my-secret-key");

    let err = serde_json::to_string(&secret)
        .expect_err("serializing a SecretString must fail loudly, not write a placeholder");
    let message = err.to_string();
    assert!(
        message.contains("refusing to serialize a SecretString"),
        "the error has to say what happened: {message}"
    );

    // The same through a struct field, which is how the wipe would actually
    // arrive: a type that happens to hold a secret, serialized for some other
    // reason entirely.
    #[derive(serde::Serialize)]
    struct Holder {
        name: String,
        key: SecretString,
    }
    let holder = Holder {
        name: "provider".to_string(),
        key: SecretString::from_str("my-secret-key"),
    };
    assert!(
        serde_json::to_string(&holder).is_err(),
        "a struct holding a secret must not serialize a placeholder over it"
    );

    // And the redacted view is still available, deliberately, through formatting.
    assert_eq!(format!("{secret}"), "[REDACTED]");
    assert_eq!(format!("{secret:?}"), "[REDACTED]");
}

/// Reading stays permissive, including the placeholder an older build could have
/// written: that file must still load so the user can see and fix the value,
/// rather than the daemon refusing to start.
#[test]
fn test_secret_string_deserialize_accepts_the_placeholder() {
    let read: SecretString = serde_json::from_str("\"[REDACTED]\"").expect("loads");
    assert_eq!(read.expose_secret(), "[REDACTED]");

    let real: SecretString = serde_json::from_str("\"sk-live-abc\"").expect("loads");
    assert_eq!(real.expose_secret(), "sk-live-abc");
}
