//! Conservative change detector for the complete persisted session layout.
//!
//! Pins storage codecs as well as serde records: a schema-only snapshot would miss
//! frame headers, artifact names, encryption and hand-written context encodings.
//! Changing any covered production source requires a store-version bump and a new
//! pin. This deliberately errs on the side of versioning compatible implementation
//! edits rather than silently advertising an incompatible worker for repin.
use llxprt_code_rs::session::STORE_VERSION;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const PINNED_VERSION: u32 = 9;
const PINNED_SHA256: &str = "6f7667a9e06956b7e2e452214173924b0a37acab70ee38a06230474b0435110c";

fn storage_sources(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if name != "tests"
                && !name.starts_with("tests_")
                && !name.ends_with("_tests.rs")
                && name != "tests.rs"
            {
                storage_sources(&path, files);
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        files.push(path.to_path_buf());
    }
}

fn fingerprint(files: &[(String, Vec<u8>)]) -> String {
    let mut hash = Sha256::new();
    for (path, bytes) in files {
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}

#[test]
fn persisted_layout_changes_require_a_store_version_bump() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    // Discover the complete storage-module trees, including newly added codecs.
    for module in [
        "session",
        "context_gateway",
        "context_ingress",
        "context_kernel",
        "context_management",
        "context_policy",
        "context_store",
        "context_txn",
        "telemetry",
        "reasoning",
    ] {
        storage_sources(&root.join("src").join(module), &mut paths);
        let file = root.join("src").join(format!("{module}.rs"));
        if file.exists() {
            paths.push(file);
        }
    }
    paths.sort();
    let files: Vec<_> = paths
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        STORE_VERSION, PINNED_VERSION,
        "pin the new version's storage sources"
    );
    assert_eq!(fingerprint(&files), PINNED_SHA256,
        "persisted layout/codecs changed: bump STORE_VERSION and replace the versioned pin; do not add a legacy reader");
}

#[test]
fn guard_detects_field_codec_and_new_artifact_changes() {
    let original = vec![(
        "record.rs".into(),
        b"struct State { version: u32 }".to_vec(),
    )];
    let pin = fingerprint(&original);
    assert_ne!(
        pin,
        fingerprint(&[(
            "record.rs".into(),
            b"struct State { version: String }".to_vec()
        )])
    );
    assert_ne!(
        pin,
        fingerprint(&[("renamed.rs".into(), original[0].1.clone())])
    );
    let mut added = original;
    added.push(("new_artifact.rs".into(), b"codec".to_vec()));
    assert_ne!(pin, fingerprint(&added));
}
