//! Session path construction and its environment-discovery seam.
//!
//! The configuration-root discovery moved here from `src/session.rs` unchanged: this module
//! owns [`config_root`], [`sessions_root`], and the safe-component rule so Phase 3 can
//! thread an injected root through this one seam. That injection is Phase 3 work; today the
//! production path still resolves the environment exactly as before, and no runner injects a
//! root.

use aes_gcm::{
    aead::{AeadCore as _, OsRng},
    Aes256Gcm,
};
use std::path::PathBuf;

/// Generates an OS-random token encoded as lowercase hexadecimal.
pub(super) fn random_token_hex() -> String {
    use std::fmt::Write as _;

    Aes256Gcm::generate_nonce(&mut OsRng)
        .iter()
        .fold(String::with_capacity(24), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

/// The configuration-root-derived sessions directory.
///
/// Phase 0 keeps this path construction here (environment discovery and the
/// `<config>/code-rs-sessions` join). Phase 3 will pass an injected root through the
/// pure construction seam while this function remains the compatibility resolver.
pub fn sessions_root() -> Result<PathBuf, String> {
    config_root().map(|root| sessions_root_from(&root))
}

/// Construct the sessions directory from an already resolved configuration root.
fn sessions_root_from(config_root: &std::path::Path) -> PathBuf {
    config_root.join("code-rs-sessions")
}

/// The configuration root for this run: the same value the profile layer resolves.
///
/// This compatibility wrapper preserves current environment discovery. Phase 3 will
/// resolve the root once and thread it through profile and session construction.
pub fn config_root() -> Result<PathBuf, String> {
    crate::profile::std_profile_dir()
}

/// A safe identifier is a bounded single path component of `[A-Za-z0-9_-]`.
pub fn is_safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_root_joins_an_injected_config_root_without_touching_the_environment() {
        let root = std::path::Path::new("/configuration-root");
        assert_eq!(sessions_root_from(root), root.join("code-rs-sessions"));
    }
}
