use super::credentials::{
    parse_credential, Clock, CodexCredential, CredentialError, CredentialSource, SystemClock,
};
use crate::config::ConfigHomeRoot;
use serde_json::Value;
use std::path::PathBuf;

mod protocol;
mod store;

pub(crate) struct LocalCredentialSource {
    root: PathBuf,
}

impl LocalCredentialSource {
    pub(crate) fn new(root: &ConfigHomeRoot) -> Self {
        Self {
            root: root.as_path().to_owned(),
        }
    }
}

impl CredentialSource for LocalCredentialSource {
    fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        let store = store::LockedStore::open(&self.root)?;
        let bytes = store.read()?.ok_or_else(|| {
            CredentialError::local("no token file; run --localoauth --oauth-login to sign in")
        })?;
        load_or_refresh(&store, &bytes, clock, |previous| {
            block_on(protocol::Protocol::production()?.refresh(previous, clock))
        })
    }
}

fn load_or_refresh(
    store: &store::LockedStore,
    bytes: &[u8],
    clock: &dyn Clock,
    refresh: impl FnOnce(&Value) -> Result<Vec<u8>, CredentialError>,
) -> Result<CodexCredential, CredentialError> {
    // Validate the complete stored shape before allowing an expired token to be refreshed.
    let expiry = super::credentials::validate_stored_credential(bytes)
        .map_err(|_| CredentialError::local("invalid token file; login again"))?;
    let previous: Value =
        serde_json::from_slice(bytes).map_err(|_| CredentialError::local("invalid token file"))?;
    let minimum = clock
        .unix_seconds()?
        .checked_add(super::credentials::CREDENTIAL_EXPIRY_SKEW_SECONDS)
        .ok_or_else(|| CredentialError::local("clock overflow"))?;
    if expiry > minimum {
        return parse_credential(bytes, clock)
            .map_err(|_| CredentialError::local("invalid token file"));
    }
    // The caller retains the stable lock through network refresh and publication. Waiters re-read.
    let updated = refresh(&previous)?;
    let credential = parse_credential(&updated, clock)
        .map_err(|_| CredentialError::local("invalid refreshed token"))?;
    store.write(&updated)?;
    Ok(credential)
}

pub(crate) fn login() -> Result<(), String> {
    let root = ConfigHomeRoot::discover()?;
    login_in(&root, &SystemClock).map_err(|error| error.to_string())
}

fn login_in(root: &ConfigHomeRoot, clock: &dyn Clock) -> Result<(), CredentialError> {
    let before = store::LockedStore::open(root.as_path())?.read()?;
    let protocol = protocol::Protocol::production()?;
    let bytes = block_on(protocol.login(clock))?;
    let store = store::LockedStore::open(root.as_path())?;
    publish_login(&store, before.as_deref(), &bytes, clock)
}

fn publish_login(
    store: &store::LockedStore,
    before: Option<&[u8]>,
    bytes: &[u8],
    clock: &dyn Clock,
) -> Result<(), CredentialError> {
    if store.read()?.as_deref() != before {
        return Err(CredentialError::local(
            "token changed during login; concurrent update retained, start login again if needed",
        ));
    }
    parse_credential(bytes, clock).map_err(|_| CredentialError::local("invalid login token"))?;
    store.write(bytes)
}

fn block_on<F, T>(future: F) -> Result<T, CredentialError>
where
    F: std::future::Future<Output = Result<T, CredentialError>>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CredentialError::local("cannot start OAuth runtime"))?
        .block_on(future)
}

#[cfg(test)]
mod tests;
