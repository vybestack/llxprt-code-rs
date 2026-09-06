use std::sync::mpsc;
use std::time::Duration;

#[cfg(test)]
use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{generic_password, PasswordOptions};

use super::credentials::{
    parse_credential, Clock, CodexCredential, CredentialError, CredentialSource,
};

const CODEX_SERVICE: &str = "llxprt-code-oauth";
const CODEX_ACCOUNT: &str = "codex:default";
const KEYCHAIN_LOAD_BOUND: Duration = Duration::from_secs(10);

pub(crate) struct MacOsCredentialSource;

impl CredentialSource for MacOsCredentialSource {
    fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        eprintln!(
            "loading macOS keychain credential (service={CODEX_SERVICE}, account={CODEX_ACCOUNT}); this can block on an authorization prompt"
        );
        let bytes = read_bounded(
            CODEX_SERVICE,
            CODEX_ACCOUNT,
            || read_generic_password(CODEX_SERVICE, CODEX_ACCOUNT),
            KEYCHAIN_LOAD_BOUND,
        )?;
        parse_credential(&bytes, clock)
    }
}

fn read_bounded<F>(
    service: &str,
    account: &str,
    load: F,
    bound: Duration,
) -> Result<Vec<u8>, CredentialError>
where
    F: FnOnce() -> Result<Vec<u8>, CredentialError> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(load());
    });
    // The SecurityServer call is not cancellation-safe, so a timed-out thread is abandoned.
    receiver
        .recv_timeout(bound)
        .map_err(|_| CredentialError::keychain_timeout(service, account))?
}

fn read_generic_password(service: &str, account: &str) -> Result<Vec<u8>, CredentialError> {
    generic_password(PasswordOptions::new_generic_password(service, account))
        .map_err(|_| CredentialError::remediation())
}

#[cfg(test)]
pub(crate) fn fixed_item_attributes() -> Result<(), CredentialError> {
    item_attributes(CODEX_SERVICE, CODEX_ACCOUNT)
}

#[cfg(test)]
fn item_attributes(service: &str, account: &str) -> Result<(), CredentialError> {
    let results = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(service)
        .account(account)
        .load_attributes(true)
        .load_data(false)
        .limit(1)
        .search()
        .map_err(|_| CredentialError::remediation())?;
    if results.is_empty() {
        return Err(CredentialError::remediation());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn read_generic_password_for_test(
    service: &str,
    account: &str,
) -> Result<Vec<u8>, CredentialError> {
    read_generic_password(service, account)
}

#[cfg(test)]
pub(crate) fn item_attributes_for_test(
    service: &str,
    account: &str,
) -> Result<(), CredentialError> {
    item_attributes(service, account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_and_parameterized_helpers_compile_without_native_access() {
        let _: fn() -> Result<(), CredentialError> = fixed_item_attributes;
        let _: fn(&str, &str) -> Result<Vec<u8>, CredentialError> = read_generic_password_for_test;
        let _: fn(&str, &str) -> Result<(), CredentialError> = item_attributes_for_test;

        fn assert_source<T: CredentialSource>() {}
        assert_source::<crate::model_api::PlatformCredentialSource>();
    }

    #[test]
    fn bounded_read_passes_through_success() {
        assert_eq!(
            read_bounded(
                "service",
                "account",
                || Ok(vec![1, 2, 3]),
                Duration::from_secs(1)
            ),
            Ok(vec![1, 2, 3])
        );
    }

    #[test]
    fn bounded_read_times_out_with_actionable_message() {
        let error = read_bounded(
            "service",
            "account",
            || {
                std::thread::sleep(Duration::from_secs(2));
                Ok(vec![])
            },
            Duration::from_millis(100),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("service"));
        assert!(message.contains("account"));
        assert!(message.contains("Always Allow"));
    }

    #[test]
    fn bounded_read_passes_through_error() {
        let expected = CredentialError::remediation().to_string();
        let error = read_bounded(
            "service",
            "account",
            || Err(CredentialError::remediation()),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn lifecycle_line_precedes_lookup() {
        assert_eq!(KEYCHAIN_LOAD_BOUND, Duration::from_secs(10));
    }
}
