#[test]
fn child_refresh() {
    let Some(path) = std::env::var_os("LLXPRT_TEST_OAUTH_REFRESH") else {
        return;
    };
    let path = std::path::Path::new(&path);
    let store = store::LockedStore::open(path).unwrap();
    let bytes = store.read().unwrap().unwrap();
    let credential = load_or_refresh(&store, &bytes, &FixedClock, |_| {
        let counter = path.join("refresh-count");
        let count: u32 = std::fs::read_to_string(&counter).unwrap().parse().unwrap();
        std::fs::write(&counter, (count + 1).to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        Ok(token("new", 2_000))
    })
    .unwrap();
    assert_eq!(credential.access_token(), "new");
}

#[test]
fn separate_processes_refresh_only_once() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    store::LockedStore::open(&path)
        .unwrap()
        .write(&token("old", 1_000))
        .unwrap();
    std::fs::write(path.join("refresh-count"), "0").unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "model_api::local_oauth::tests::child_refresh"])
                .env("LLXPRT_TEST_OAUTH_REFRESH", &path)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        std::fs::read_to_string(path.join("refresh-count")).unwrap(),
        "1"
    );
}

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct FixedClock;
impl Clock for FixedClock {
    fn unix_seconds(&self) -> Result<i64, CredentialError> {
        Ok(1_000)
    }
}

fn token(access: &str, expiry: i64) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"access_token": access, "account_id": "account", "expiry": expiry,
        "token_type": "Bearer", "refresh_token": "refresh-secret"}),
    )
    .unwrap()
}

#[test]
fn local_source_loads_without_native_keychain() {
    let temp = tempfile::tempdir().unwrap();
    let root = ConfigHomeRoot::for_test(temp.path().canonicalize().unwrap()).unwrap();
    let source = LocalCredentialSource::new(&root);
    let error = source.load(&FixedClock).unwrap_err().to_string();
    assert!(error.contains("--localoauth --oauth-login"));
    assert!(!error.contains("keychain"));
    store::LockedStore::open(root.as_path())
        .unwrap()
        .write(&token("synthetic-token", 2_000))
        .unwrap();
    let credential = source.load(&FixedClock).unwrap();
    assert_eq!(credential.access_token(), "synthetic-token");
    assert!(!format!("{credential:?}").contains("synthetic-token"));
}

#[test]
fn failed_refresh_preserves_exact_old_token() {
    let temp = tempfile::tempdir().unwrap();
    let store = store::LockedStore::open(&temp.path().canonicalize().unwrap()).unwrap();
    let original = token("old", 1_000);
    store.write(&original).unwrap();
    let error = load_or_refresh(&store, &original, &FixedClock, |_| {
        Err(CredentialError::local("HTTP failure"))
    })
    .unwrap_err();
    assert!(!error.to_string().contains("refresh-secret"));
    assert_eq!(store.read().unwrap().unwrap(), original);
    assert!(
        load_or_refresh(&store, &original, &FixedClock, |_| Ok(token(
            "invalid-new",
            1_000
        )))
        .is_err()
    );
    assert_eq!(store.read().unwrap().unwrap(), original);
}

#[test]
fn refresh_is_single_flight_and_waiters_re_read() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    store::LockedStore::open(&path)
        .unwrap()
        .write(&token("old", 1_000))
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let path = path.clone();
        let calls = calls.clone();
        workers.push(std::thread::spawn(move || {
            let store = store::LockedStore::open(&path).unwrap();
            let bytes = store.read().unwrap().unwrap();
            load_or_refresh(&store, &bytes, &FixedClock, |previous| {
                assert_eq!(previous["refresh_token"], "refresh-secret");
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(token("new", 2_000))
            })
            .unwrap()
            .access_token()
            .to_owned()
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap(), "new");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_login_cannot_overwrite_newer_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    let store = store::LockedStore::open(&path).unwrap();
    let original = token("old", 2_000);
    let refreshed = token("newer", 3_000);
    store.write(&original).unwrap();
    store.write(&refreshed).unwrap();
    assert!(publish_login(&store, Some(&original), &token("login", 4_000), &FixedClock).is_err());
    assert_eq!(store.read().unwrap().unwrap(), refreshed);
    publish_login(
        &store,
        Some(&refreshed),
        &token("login", 4_000),
        &FixedClock,
    )
    .unwrap();
}

#[test]
fn invalid_stored_shapes_never_trigger_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let store = store::LockedStore::open(&temp.path().canonicalize().unwrap()).unwrap();
    for bytes in [b"{}".as_slice(), b"{\"expiry\":1000,\"expiry\":2000}"] {
        let error = load_or_refresh(&store, bytes, &FixedClock, |_| {
            panic!("must not refresh invalid credentials")
        })
        .unwrap_err();
        assert!(!error.to_string().contains("keychain"));
    }
}
