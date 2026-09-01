use llxprt_code_rs::envelope::{schema_bytes, Envelope};
use serde_json::Value;
use std::path::{Path, PathBuf};

const WINDOWS_MACHINE_PREFIX: &str = "C:\\Users\\";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn documents(kind: &str) -> Vec<PathBuf> {
    let dir = root().join("tests/fixtures/envelope").join(kind);
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn published_schema_is_byte_for_byte_current() {
    let tracked = std::fs::read(root().join("docs/envelope.schema.json")).unwrap();
    assert_eq!(
        tracked,
        schema_bytes(),
        "schema drift: run `cargo xtask envelope-schema`"
    );
}

#[test]
fn schema_is_valid_and_corpus_matches_shared_serde_types() {
    let schema: Value = serde_json::from_slice(&schema_bytes()).unwrap();
    jsonschema::draft202012::meta::validate(&schema).expect("generated schema meta-validates");
    let validator = jsonschema::draft202012::new(&schema).expect("generated schema compiles");

    for path in documents("positive") {
        let document = read_json(&path);
        assert!(
            validator.is_valid(&document),
            "schema rejected {}: {:?}",
            path.display(),
            validator.iter_errors(&document).collect::<Vec<_>>()
        );
        serde_json::from_value::<Envelope>(document)
            .unwrap_or_else(|error| panic!("serde rejected {}: {error}", path.display()));
    }
    for path in documents("negative") {
        let document = read_json(&path);
        assert!(
            !validator.is_valid(&document),
            "schema accepted negative {}",
            path.display()
        );
        assert!(
            serde_json::from_value::<Envelope>(document).is_err(),
            "serde accepted negative {}",
            path.display()
        );
    }
}

fn fixture_safety_error(path: &Path) -> Option<String> {
    let machine_prefixes = ["/Users/", "/home/", WINDOWS_MACHINE_PREFIX];
    let secret_sentinels = ["sk-", "api_key", "auth-key", "BEGIN PRIVATE KEY"];
    let text = std::fs::read_to_string(path).unwrap();
    machine_prefixes
        .into_iter()
        .chain(secret_sentinels)
        .find(|forbidden| text.contains(forbidden))
        .map(str::to_owned)
}

#[test]
fn fixture_corpus_contains_no_secret_or_machine_path_capture() {
    let root = root().join("tests/fixtures/envelope");
    let mut pending = vec![root];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
            continue;
        }
        assert_eq!(
            fixture_safety_error(&path),
            None,
            "{} contains forbidden fixture data",
            path.display()
        );
    }
}

#[test]
fn fixture_scanner_rejects_a_windows_machine_path() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), r"fixture captured C:\Users\alice\project").unwrap();
    assert_eq!(
        fixture_safety_error(temp.path()).as_deref(),
        Some(WINDOWS_MACHINE_PREFIX)
    );
}
