//! Named provider keys: the `auth-key-name` profile reference (issue 6).
//!
//! A profile names its provider key with `ephemeralSettings.auth-key-name`. The
//! name resolves through exactly two layers, mirroring the TypeScript client's
//! resolution for a named key:
//!
//! 1. the credential env selector `LLXPRT_PROVIDER_KEY_<NAME>`, where `<NAME>` is
//!    the uppercased name with `-` and `.` folded to `_`;
//! 2. the secure store: service `llxprt-code-provider-keys`, account `<name>`
//!    (the native keychain on macOS, unavailable elsewhere).
//!
//! Resolution is value-free on failure: a missing key reports the fixed
//! [`RESOLUTION_FAILURE`] diagnostic, which never carries the name, the env
//! selector, or the secret.

use std::fmt;

/// The fixed, value-free failure for an unresolvable named provider key. The name
/// and the secret are credential surfaces; neither ever travels.
pub(crate) const RESOLUTION_FAILURE: &str = "auth-key-name could not be resolved; set LLXPRT_PROVIDER_KEY_<NAME> or store the key under the llxprt-code-provider-keys secure-store account";

/// The folded env selector for a named provider key: `LLXPRT_PROVIDER_KEY_`
/// plus the uppercased name with `-` and `.` folded to `_` (so `zai`, `zAi`, and
/// `friendli-glm` select `LLXPRT_PROVIDER_KEY_ZAI` and
/// `LLXPRT_PROVIDER_KEY_FRIENDLI_GLM`). The selector never appears in an error.
pub(crate) fn env_selector(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|c| match c {
            '-' | '.' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect();
    format!("LLXPRT_PROVIDER_KEY_{folded}")
}

/// Read the named provider key from the process environment. The secret is held for
/// the transport only; the value never appears in an error or a diagnostic.
pub(crate) fn from_env(name: &str) -> Option<String> {
    let value = std::env::var_os(env_selector(name))?;
    let value = value.into_string().ok()?;
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn from_keychain(name: &str) -> Option<String> {
    super::macos_keychain::read_named_provider_key(name)
}

/// The secure store is the native macOS keychain; on every other platform only the
/// env selector resolves a named key.
#[cfg(not(target_os = "macos"))]
pub(crate) fn from_keychain(_name: &str) -> Option<String> {
    None
}

/// A named provider key that could not be resolved through either layer. The fixed
/// diagnostic never carries the name, the env selector, or the secret.
#[derive(Debug)]
pub(crate) struct UnresolvedNamedKey;

impl fmt::Display for UnresolvedNamedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(RESOLUTION_FAILURE)
    }
}

impl std::error::Error for UnresolvedNamedKey {}

/// Resolve `auth-key-name`: the credential env selector first, then the secure
/// store. Returns the bounded secret for the transport, or the fixed value-free
/// [`UnresolvedNamedKey`] diagnostic when neither layer holds the key.
pub(crate) fn resolve_named_key(name: &str) -> Result<String, UnresolvedNamedKey> {
    let key = from_env(name)
        .or_else(|| from_keychain(name))
        .ok_or(UnresolvedNamedKey)?;
    if key.len() > crate::redact::MAX_KEY_BYTES || key.as_bytes().contains(&0) {
        return Err(UnresolvedNamedKey);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector folds case and the two documented separators, so every spelling
    /// of one name selects one env var.
    #[test]
    fn env_selector_folds_case_and_separators() {
        assert_eq!(env_selector("zai"), "LLXPRT_PROVIDER_KEY_ZAI");
        assert_eq!(env_selector("zAi"), "LLXPRT_PROVIDER_KEY_ZAI");
        assert_eq!(
            env_selector("friendli-glm"),
            "LLXPRT_PROVIDER_KEY_FRIENDLI_GLM"
        );
        assert_eq!(env_selector("a.b"), "LLXPRT_PROVIDER_KEY_A_B");
    }

    /// The fixed failure diagnostic never carries the name, the selector, or a secret,
    /// and stays inside the diagnostic bound.
    #[test]
    fn resolution_failure_is_value_free_and_bounded() {
        assert_eq!(UnresolvedNamedKey.to_string(), RESOLUTION_FAILURE);
        assert!(!RESOLUTION_FAILURE.contains("zai"));
        assert!(RESOLUTION_FAILURE.len() <= crate::redact::MAX_DIAGNOSTIC_BYTES);
    }

    /// A missing key resolves to the fixed diagnostic on every platform (the
    /// environment holds no selector and the secure store holds no such account).
    #[test]
    fn missing_named_key_reports_the_fixed_diagnostic() {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _guard = GUARD.lock().unwrap_or_else(|error| error.into_inner());
        // The folded selector for this marker is unique to this test.
        let selector = env_selector("no-such-provider-key-6");
        let previous = std::env::var_os(&selector);
        unsafe {
            std::env::remove_var(&selector);
        }
        let error = resolve_named_key("no-such-provider-key-6").expect_err("unresolved");
        assert_eq!(error.to_string(), RESOLUTION_FAILURE);
        if let Some(value) = previous {
            unsafe { std::env::set_var(&selector, value) }
        }
    }

    /// The env selector resolves and is trimmed; an over-cap value is refused with the
    /// fixed diagnostic rather than surfaced.
    #[test]
    fn env_selector_resolves_and_bounds_the_secret() {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _guard = GUARD.lock().unwrap_or_else(|error| error.into_inner());
        let selector = env_selector("env-resolved-key-6");
        let previous = std::env::var_os(&selector);
        unsafe {
            std::env::set_var(&selector, "  env-key-6  ");
        }
        assert_eq!(
            resolve_named_key("env-resolved-key-6").ok().as_deref(),
            Some("env-key-6")
        );
        unsafe {
            std::env::set_var(&selector, "x".repeat(crate::redact::MAX_KEY_BYTES + 1));
        }
        let error = resolve_named_key("env-resolved-key-6").expect_err("over-cap");
        assert_eq!(error.to_string(), RESOLUTION_FAILURE);
        match previous {
            Some(value) => unsafe { std::env::set_var(&selector, value) },
            None => unsafe { std::env::remove_var(&selector) },
        }
    }
}
