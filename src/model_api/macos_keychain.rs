#[cfg(test)]
use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{generic_password, PasswordOptions};

use super::credentials::{
    parse_credential, Clock, CodexCredential, CredentialError, CredentialSource,
};

const CODEX_SERVICE: &str = "llxprt-code-oauth";
const CODEX_ACCOUNT: &str = "codex:default";

pub(crate) struct MacOsCredentialSource;

impl CredentialSource for MacOsCredentialSource {
    fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        let bytes = read_generic_password(CODEX_SERVICE, CODEX_ACCOUNT)?;
        parse_credential(&bytes, clock)
    }
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
}
