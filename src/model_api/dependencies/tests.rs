use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::config::ConfigHomeRoot;
use crate::model_api::credentials::{CodexCredential, CredentialError};

struct FixedClock(i64);

impl Clock for FixedClock {
    fn unix_seconds(&self) -> Result<i64, CredentialError> {
        Ok(self.0)
    }
}

struct InMemorySource {
    calls: AtomicUsize,
}

impl CredentialSource for InMemorySource {
    fn load(&self, _clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(CredentialError::remediation())
    }
}

fn dependencies(
    registrations: &'static [ModelRegistration],
) -> (RuntimeDependencies, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let config_home = ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap();
    let source = Arc::new(InMemorySource {
        calls: AtomicUsize::new(0),
    });
    let dependencies = RuntimeDependencies::for_test(
        source,
        Arc::new(FixedClock(1_000)),
        config_home,
        registrations,
    );
    (dependencies, root)
}

#[test]
fn production_registration_slice_is_exact_and_typed() {
    let root = tempfile::tempdir().unwrap();
    let dependencies = RuntimeDependencies::new(
        Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(FixedClock(1_000)),
        ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
    );
    let targets = dependencies
        .registrations()
        .iter()
        .map(|registration| registration.target)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 7);
    assert!(targets.contains(&ModelTarget {
        provider: ProviderId::Anthropic,
        api: ModelApi::AnthropicMessages,
        transport: TransportKind::Http,
    }));
    assert!(targets.contains(&ModelTarget {
        provider: ProviderId::Codex,
        api: ModelApi::Responses,
        transport: TransportKind::Http,
    }));
    assert!(targets.contains(&ModelTarget {
        provider: ProviderId::OpenAiResponses,
        api: ModelApi::Responses,
        transport: TransportKind::Http,
    }));
    assert!(targets.contains(&ModelTarget {
        provider: ProviderId::OpenAi,
        api: ModelApi::Responses,
        transport: TransportKind::Http,
    }));
}

#[test]
fn test_constructor_injects_all_dependencies_and_one_root() {
    static TEST_REGISTRATIONS: &[ModelRegistration] = &[ModelRegistration {
        target: ModelTarget {
            provider: ProviderId::OpenAi,
            api: ModelApi::ChatCompletions,
            transport: TransportKind::Http,
        },
        constructor: ConstructorKind::OpenAiChat,
    }];
    let (dependencies, root) = dependencies(TEST_REGISTRATIONS);

    assert_eq!(dependencies.clock().unix_seconds().unwrap(), 1_000);
    assert_eq!(dependencies.config_home().as_path(), root.path());
    assert_eq!(dependencies.registrations(), TEST_REGISTRATIONS);
    assert!(dependencies
        .credential_source()
        .load(dependencies.clock())
        .is_err());
}

#[test]
fn config_home_rejects_relative_and_empty_paths() {
    assert!(ConfigHomeRoot::for_test(PathBuf::new()).is_err());
    assert!(ConfigHomeRoot::for_test(PathBuf::from("relative/path")).is_err());
}
