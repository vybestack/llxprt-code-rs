pub(crate) mod credentials;
pub(crate) mod dependencies;
pub(crate) mod identity;
#[cfg(target_os = "macos")]
pub(crate) mod macos_keychain;
#[cfg(all(test, target_os = "macos"))]
mod operator_protocol;
pub(crate) mod settings;

#[cfg(target_os = "macos")]
pub(crate) type PlatformCredentialSource = macos_keychain::MacOsCredentialSource;
#[cfg(not(target_os = "macos"))]
pub(crate) type PlatformCredentialSource = credentials::UnsupportedCredentialSource;
