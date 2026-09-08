//! Cross-process coverage of the production OS-entropy paths (issue 127).

use super::*;
use crate::context_store::vault::Vault;
use sha2::{Digest, Sha256};

const CHILD_ROOT: &str = "LLXPRT_TEST_VAULT_ENTROPY_ROOT";

#[derive(serde::Serialize, serde::Deserialize)]
struct Sample {
    key_fingerprint: [u8; 32],
    open_prefix: u32,
    restore_prefix: u32,
}

fn sample_in_child(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    // Identical session id in isolated roots; no real user store or keychain.
    let id = SessionId::parse("vault-entropy-process").unwrap();
    let store = SessionStore::load_at(&id, root).unwrap();
    let key = context_persist::ensure_vault_key(&store).unwrap();
    let key_path = store.session_dir.join("context-vault-key");
    let metadata = std::fs::metadata(key_path).unwrap();
    assert_eq!(metadata.len(), 32);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let reopened = context_persist::ensure_vault_key(&store).unwrap();
    assert!(key == reopened, "reopening must retain the session key");

    let mut vault = Vault::open(&key);
    let handle = vault.put(b"cross-process entropy probe", "probe").unwrap();
    let snapshot = vault.snapshot();
    let open_prefix = snapshot.nonce_prefix;
    vault.restore(snapshot).unwrap();
    assert_eq!(vault.get(&handle).unwrap(), b"cross-process entropy probe");
    let sample = Sample {
        key_fingerprint: Sha256::digest(key).into(),
        open_prefix,
        restore_prefix: vault.snapshot().nonce_prefix,
    };
    // Only a one-way fingerprint crosses the test seam; raw key bytes stay
    // in memory and in the production 0600 artifact inside this private root.
    std::fs::write(
        root.join("sample.json"),
        serde_json::to_vec(&sample).unwrap(),
    )
    .unwrap();
}

#[test]
fn vault_keys_and_nonce_prefixes_differ_across_processes() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        sample_in_child(std::path::Path::new(&root));
        return;
    }
    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let mut samples: Vec<Sample> = Vec::new();
    for _ in 0..4 {
        let root = tempfile::tempdir_in(&scratch).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tests::vault_entropy::vault_keys_and_nonce_prefixes_differ_across_processes",
                "--test-threads=1",
            ])
            .env_clear()
            .env(CHILD_ROOT, root.path())
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "entropy subprocess failed");
        // Missing/zero-test matches cannot pass: only the child test writes this.
        let bytes = std::fs::read(root.path().join("sample.json")).unwrap();
        let sample: Sample = serde_json::from_slice(&bytes).unwrap();
        assert!(
            samples
                .iter()
                .all(|previous| previous.key_fingerprint != sample.key_fingerprint),
            "separate processes repeated a vault key fingerprint"
        );
        samples.push(sample);
    }
    // A 32-bit prefix can collide. Do not require pairwise uniqueness: four
    // independent draws are all equal with probability 2^-96 per path. This
    // detects a constant/replayed prefix, not a mathematical uniqueness proof
    // or a statistical certification of the OS entropy source.
    assert!(
        samples
            .iter()
            .any(|sample| sample.open_prefix != samples[0].open_prefix),
        "separate processes all repeated the open prefix"
    );
    assert!(
        samples
            .iter()
            .any(|sample| sample.restore_prefix != samples[0].restore_prefix),
        "separate processes all repeated the restore prefix"
    );
}
