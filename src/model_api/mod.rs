mod anthropic_backend;
pub(crate) mod credentials;
pub(crate) mod dependencies;
pub(crate) mod interpret;
#[cfg(target_os = "macos")]
pub(crate) mod macos_keychain;
mod model_params;
pub(crate) mod model_registry;
pub(crate) mod registry;
mod responses_backend;
pub(crate) mod settings;
pub(crate) mod target;

#[cfg(all(test, target_os = "macos"))]
pub(crate) type PlatformCredentialSource = macos_keychain::MacOsCredentialSource;

#[cfg(test)]
mod tests;
