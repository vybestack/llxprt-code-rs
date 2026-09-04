mod anthropic_backend;
pub(crate) mod credentials;
pub(crate) mod dependencies;
#[cfg(target_os = "macos")]
pub(crate) mod macos_keychain;
pub(crate) mod provider_keys;
pub(crate) mod registry;
mod responses_backend;
pub(crate) mod settings;
pub(crate) mod target;

#[cfg(all(test, target_os = "macos"))]
pub(crate) type PlatformCredentialSource = macos_keychain::MacOsCredentialSource;

#[cfg(test)]
mod tests;
