use super::*;

#[test]
fn cargo_fixtures_ignore_ancestor_manifests() {
    for ancestor in [
        "[package]\nname='unrelated'\nversion='0.1.0'\n", // invalid: no targets
        "[workspace]\nmembers=['missing-member']\n",      // unrelated workspace
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), ancestor).unwrap();
        let original = root.path().join("original");
        std::fs::create_dir(&original).unwrap();
        write_encryption_good(&original);
        let ws = cap(&original);
        let produced = root.path().join("produced");
        assert!(materialize_sources(&ws, &produced));
        assert!(
            build_and_test_consumer(&root.path().join("consumer"), &produced, "filecrypt"),
            "consumer inherited ancestor {ancestor:?}"
        );
        assert_eq!(
            probe_crypto_removal(&root.path().join("pruned"), &ws, "filecrypt"),
            RemovalProbe::ExpectedCryptoCompileFailure,
            "removal probe inherited ancestor {ancestor:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("Cargo.toml")).unwrap(),
            ancestor
        );
    }
}

#[test]
fn fixture_manifest_detaches_old_workspace_but_preserves_local_declarations() {
    let root = tempfile::tempdir().unwrap();
    let text = "[package]\nname='standalone'\nversion='0.1.0'\nworkspace='../unrelated'\n\
                [workspace.dependencies]\naes-gcm='0.10'\n\
                [dependencies]\naes-gcm={workspace=true}\n";
    assert!(cargo_fixture::write_manifest(root.path(), text));
    let written = std::fs::read_to_string(root.path().join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&written).unwrap();
    assert!(manifest["package"].get("workspace").is_none());
    assert_eq!(
        manifest["workspace"]["dependencies"]["aes-gcm"].as_str(),
        Some("0.10")
    );
    assert_eq!(
        manifest["dependencies"]["aes-gcm"]["workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(manifest["package"]["name"].as_str(), Some("standalone"));
    assert!(!cargo_fixture::write_manifest(
        root.path(),
        "not a manifest"
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("Cargo.toml")).unwrap(),
        written
    );
}

#[test]
fn failing_consumer_and_unrelated_removal_errors_stay_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    std::fs::create_dir(&original).unwrap();
    write_encryption_good(&original);
    std::fs::write(
        original.join("src/lib.rs"),
        "compile_error!(\"fixture failure sentinel\");",
    )
    .unwrap();
    let produced = root.path().join("produced");
    let ws = cap(&original);
    assert!(materialize_sources(&ws, &produced));
    assert!(!build_and_test_consumer(
        &root.path().join("consumer"),
        &produced,
        "filecrypt"
    ));
    let pruned = root.path().join("pruned");
    assert_eq!(
        probe_crypto_removal(&pruned, &ws, "filecrypt"),
        RemovalProbe::Inconclusive
    );
    let outcome = cargo_fixture::run("cargo", &pruned, PROBE_MAX_OUTPUT).unwrap();
    assert_ne!(outcome.status, Some(0));
    assert!(!outcome.timed_out);
    let diagnostic = cargo_fixture::diagnostic(&pruned, &outcome);
    assert!(
        diagnostic.contains("fixture failure sentinel"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("status=Some(101)"), "{diagnostic}");
    assert!(cargo_fixture::run("/definitely/missing/cargo", root.path(), 1024).is_none());
}

#[test]
fn nested_cargo_diagnostics_include_bounded_stream_tails_and_failure_metadata() {
    let outcome = process::CmdOutcome {
        status: Some(101),
        timed_out: true,
        stdout: [vec![b'x'; 10_000], b"stdout sentinel".to_vec()].concat(),
        stderr: [vec![b'y'; 10_000], b"manifest failure sentinel".to_vec()].concat(),
        stdout_truncated: true,
        stderr_truncated: false,
        combined_truncated: true,
    };
    let text = cargo_fixture::diagnostic(Path::new("fixture/consumer"), &outcome);
    for expected in [
        "cargo test --offline --manifest-path ./Cargo.toml",
        "fixture/consumer",
        "status=Some(101)",
        "timed_out=true",
        "stdout_truncated=true",
        "stderr_truncated=false",
        "combined_truncated=true",
        "stdout sentinel",
        "manifest failure sentinel",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(text.len() < 9000, "diagnostic was not bounded");
}
