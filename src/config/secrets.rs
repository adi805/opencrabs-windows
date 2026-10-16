//! Secure secret management
//!
//! This module provides secure handling of sensitive data like API keys,
//! ensuring they are properly zeroized from memory when dropped.

use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A secure string that zeroizes its contents on drop
///
/// This type should be used for any sensitive data like API keys,
/// passwords, or tokens to ensure they are properly cleared from memory.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretString {
    inner: String,
}

impl SecretString {
    /// Create a new SecretString from a String
    pub fn new(value: String) -> Self {
        Self { inner: value }
    }

    /// Create a new SecretString from a &str
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Self {
        Self {
            inner: value.to_string(),
        }
    }

    /// Get a reference to the inner string
    ///
    /// # Security Warning
    /// Use with caution! This exposes the sensitive data.
    /// Avoid logging or displaying the returned value.
    pub fn expose_secret(&self) -> &str {
        &self.inner
    }

    /// Check if the secret is empty
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get the length of the secret
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Refuses to serialize, rather than writing a placeholder (F-CFG-01).
///
/// This used to return `serialize_str("[REDACTED]")`, on the reasoning that a
/// secret should never reach a file. The effect was the opposite of safe: the
/// placeholder is a perfectly ordinary string, so a struct holding a
/// `SecretString` serialized cleanly and wrote `[REDACTED]` where the real key
/// had been. Nothing errored, and the next load read the placeholder back as a
/// literal value (the `Deserialize` impl below accepts any string), so the key
/// was gone and the only symptom was a provider that stopped authenticating.
///
/// An error is louder and costs nothing: no writer in the tree serializes a type
/// holding a `SecretString` (the keys writer goes through `toml_edit` per key),
/// so nothing fails today, and the first struct that tries gets a message instead
/// of a wiped credential. A caller that wants a redacted view for a log or a
/// diagnostic can format the value, which is what [`fmt::Display`] and
/// [`fmt::Debug`] are for.
impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Consume the serializer without producing a value, so a serializer that
        // requires a call cannot be left half-written.
        let _ = serializer;
        Err(serde::ser::Error::custom(
            "refusing to serialize a SecretString: it would either leak the secret or \
             overwrite it with a placeholder. Format the value for a redacted view, or \
             call expose_secret() where the real secret is genuinely wanted.",
        ))
    }
}

/// Reads any string, including the placeholder this type refuses to write.
///
/// Deliberately permissive, and the asymmetry with `Serialize` is the point: a
/// key file that already contains `[REDACTED]` (from a build before F-CFG-01 was
/// fixed) still loads, so the user can see the broken value and replace it,
/// rather than the daemon refusing to start. Rejecting it here would turn a
/// recoverable misconfiguration into a boot failure.
impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(SecretString::new(s))
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        SecretString::new(s)
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        SecretString::from_str(s)
    }
}
