use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::credentials::{Clock, CredentialSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigHomeRoot(PathBuf);

impl ConfigHomeRoot {
    pub(crate) fn discover() -> Result<Self, String> {
        Self::from_path(crate::profile::std_profile_dir()?)
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err("configuration home must be a nonempty absolute path".to_string());
        }
        Ok(Self(path))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf) -> Result<Self, String> {
        Self::from_path(path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelRegistration {
    _private: (),
}

static PRODUCTION_REGISTRATIONS: &[ModelRegistration] = &[];

pub(crate) struct RuntimeDependencies {
    credential_source: Arc<dyn CredentialSource>,
    clock: Arc<dyn Clock>,
    config_home: ConfigHomeRoot,
    registrations: &'static [ModelRegistration],
}

impl RuntimeDependencies {
    pub(crate) fn new(
        credential_source: Arc<dyn CredentialSource>,
        clock: Arc<dyn Clock>,
        config_home: ConfigHomeRoot,
    ) -> Self {
        Self {
            credential_source,
            clock,
            config_home,
            registrations: PRODUCTION_REGISTRATIONS,
        }
    }

    pub(crate) fn credential_source(&self) -> &dyn CredentialSource {
        self.credential_source.as_ref()
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub(crate) fn config_home(&self) -> &ConfigHomeRoot {
        &self.config_home
    }

    pub(crate) fn registrations(&self) -> &'static [ModelRegistration] {
        self.registrations
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        credential_source: Arc<dyn CredentialSource>,
        clock: Arc<dyn Clock>,
        config_home: ConfigHomeRoot,
        registrations: &'static [ModelRegistration],
    ) -> Self {
        Self {
            credential_source,
            clock,
            config_home,
            registrations,
        }
    }
}

#[cfg(test)]
mod tests;
