use super::*;

/// A custom codec that constructs an allowed crypto type (and even calls an authenticating
/// operation on it) but **discards** the result while returning its own synthetic bytes is
/// not crypto: constructors and discarded calls never become encrypt/decrypt evidence.
#[test]
fn encryption_discarded_constructor_and_call_fail() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "fn xor(pw: &[u8], m: &[u8]) -> Vec<u8> { m.iter().enumerate().map(|(i, b)| b.wrapping_add(pw[i % pw.len().max(1)])).collect() }\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    use aes_gcm::{Key, Aes256Gcm};\n    let k = Key::<Aes256Gcm>::default();\n    let _c = Aes256Gcm::new(&k);\n    let _ = Aes256Gcm::new(&aes_gcm::Key::<Aes256Gcm>::default());\n    Ok(xor(pw.as_bytes(), m))\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    Ok(c.iter().map(|b| b.wrapping_sub(pw.as_bytes()[pw.len().max(1) - 1])).collect())\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a discarded crypto constructor/call is never returned encrypt/decrypt evidence"
    );
}

/// A real operation placed only in a provably unreachable branch (`if false`) while the
/// function returns its own synthetic codec never passes: the operation never executes and
/// its result never reaches a returned value.
#[test]
fn encryption_unreachable_branch_never_counts() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "fn xor(pw: &[u8], m: &[u8]) -> Vec<u8> { m.iter().enumerate().map(|(i, b)| b.wrapping_add(pw[i % pw.len().max(1)])).collect() }\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    if false {\n        let cipher = aes_gcm::Aes256Gcm::new(&aes_gcm::Key::<aes_gcm::Aes256Gcm>::default());\n        return cipher.encrypt(&aes_gcm::aead::Payload::from(m), &m).map_err(|_| \"x\".to_string());\n    }\n    Ok(xor(pw.as_bytes(), m))\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    Ok(c.iter().map(|b| b.wrapping_sub(pw.as_bytes()[pw.len().max(1) - 1])).collect())\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "an operation in an unreachable branch is never returned evidence"
    );
}

/// A codec whose `encrypt` returns a real authenticated encrypt but whose `decrypt` returns
/// only its own synthetic reverse is one-direction fake: each direction is analyzed
/// independently, so the single real side can never pass the crate.
#[test]
fn encryption_one_direction_crypto_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use aes_gcm::{aead::{Aead, AeadCore, KeyInit, OsRng}, Aes256Gcm, Key};\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());\n    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);\n    Ok(cipher.encrypt(&nonce, m).map_err(|_| \"e\".to_string()))\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    Ok(c.iter().rev().copied().collect())\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "real encrypt with a hand-rolled reverse decrypt must fail"
    );
}

/// A `let _ = cipher.encrypt(..)` that discards the authenticated result while the
/// function returns its own synthetic bytes is never returned evidence: the discard binds
/// nothing and no taint can flow into the returned value.
#[test]
fn encryption_let_underscore_discard_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};\nfn seed() -> Vec<u8> { vec![0u8; 24] }\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());\n    let _ = cipher.encrypt(&Nonce::default(), m);\n    Ok(m.iter().map(|b| b.wrapping_add(1)).collect())\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());\n    let _ = cipher.decrypt(&Nonce::default(), c);\n    Ok(c.iter().map(|b| b.wrapping_sub(1)).collect())\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a discarded `let _` authenticated call is never returned evidence"
    );
}

/// The exact negative fixture: a genuinely working XOR/DefaultHasher/tag codec whose
/// encrypted/decrypt internally invoke the **real** AES-GCM operations but discard their
/// results through tuple projection, returning only custom sibling elements. The
/// behavior is real (it roundtrips, rejects wrong passwords, rejects tampering, mixes
/// per-call salts, carries AEAD overhead) but the exported encrypt/decrypt returns never
/// descend from the acknowledged operations: tuple/struct sibling elements, decoy calls,
/// and custom returned values must never fabricate evidence.
#[test]
fn encryption_tuple_projection_custom_codec_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};\nuse aes_gcm::{Aes256Gcm, Key, Nonce};\nuse std::collections::hash_map::DefaultHasher;\nuse std::hash::Hash;\nuse std::sync::atomic::{AtomicU64, Ordering};\n\nstatic COUNTER: AtomicU64 = AtomicU64::new(1);\n\nfn salt_bytes() -> Vec<u8> {\n    let n = COUNTER.fetch_add(1, Ordering::Relaxed);\n    (0..24).map(|i| (n.wrapping_mul(i as u64 + 1).wrapping_mul(2654435761) >> (i % 8)) as u8).collect()\n}\n\nfn fold(pw: &[u8], data: &[u8]) -> u64 {\n    let mut h = DefaultHasher::new();\n    pw.hash(&mut h);\n    data.hash(&mut h);\n    h.finish()\n}\n\nfn scramble(pw: &[u8], data: &[u8], salt: &[u8]) -> Vec<u8> {\n    data.iter().enumerate().map(|(i, b)| b.wrapping_add(pw[i % pw.len().max(1)]) ^ salt[i % salt.len().max(1)]).collect()\n}\n\nfn unscramble(pw: &[u8], data: &[u8], salt: &[u8]) -> Vec<u8> {\n    data.iter().enumerate().map(|(i, b)| b.wrapping_sub(pw[i % pw.len().max(1)]) ^ salt[i % salt.len().max(1)]).collect()\n}\n\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());\n    let (real, salt) = (cipher.encrypt(&Nonce::default(), m).map_err(|_| \"encrypt failure\".to_string())?, salt_bytes());\n    let _ = real;\n    let body = scramble(pw.as_bytes(), m, &salt);\n    let mut out = salt;\n    out.extend_from_slice(&body);\n    let t = fold(pw.as_bytes(), &body).to_le_bytes();\n    out.extend_from_slice(&t);\n    Ok(out)\n}\n\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    if c.len() < 24 + 8 { return Err(\"ciphertext too short\".to_string()); }\n    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());\n    let (salt, rest) = (&c[..24], &c[24..c.len() - 8]);\n    let (body, tag) = (&rest[..rest.len() - 8], &rest[rest.len() - 8..]);\n    let (real, _ok) = (cipher.decrypt(&Nonce::from_slice(salt), body).map_err(|_| \"decrypt failure\".to_string())?, 0u8);\n    let _ = real;\n    if fold(pw.as_bytes(), body).to_le_bytes() != tag { return Err(\"tag mismatch\".to_string()); }\n    Ok(unscramble(pw.as_bytes(), body, salt))\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "real AES-GCM calls discarded via tuple projection must never make a custom \
         DefaultHasher/XOR/tag codec pass the crate grader"
    );
    let checks = hidden_grader_checks("encryption");
    assert!(
        !checks.iter().all(|(_, c)| c(&cap(d.path()))),
        "tuple-projected codec must fail at least one hidden encryption grader"
    );
}

/// Tuple projection is value-sensitive: a custom sibling element never inherits the
/// taint of a sibling op element, and the selected op element keeps its flow.
#[test]
fn encryption_tuple_sibling_projection_does_not_taint_custom_element() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let payload = (cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?, m.to_vec());
            Ok(payload.1)
        }"#,
    ));
}

#[test]
fn encryption_tuple_selected_operation_element_keeps_flow() {
    assert!(flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let payload = (m.to_vec(), cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?);
            Ok(payload.1)
        }"#,
    ));
}

/// Struct construction and field projection are equally field-sensitive: a decoy op
/// stored in one field never taints a custom sibling field that is actually returned.
#[test]
fn encryption_struct_projection_does_not_taint_custom_field() {
    assert!(!flow_fixture(
        r#"struct Envelope { body: Vec<u8>, sealed: Vec<u8> }
        pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let e = Envelope { body: m.to_vec(), sealed: cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())? };
            Ok(e.body)
        }"#,
    ));
}

/// Array literals are projected element-wise too: `arr[1]` selects only element 1, so
/// a decoy op in another slot never blesses a custom returned element.
#[test]
fn encryption_array_index_projection_is_element_sensitive() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let arr = [cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?, m.to_vec()];
            Ok(arr[1].clone())
        }"#,
    ));
}

/// A local shadow assignment fully replaces the value: overwriting an op result with
/// custom bytes clears the operation provenance (a shadowed local never keeps stale flow).
#[test]
fn encryption_local_shadow_overwrite_clears_operation_flow() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let r = cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?;
            let r = m.to_vec();
            Ok(r)
        }"#,
    ));
}

/// An unknown projection (an out-of-range tuple index) fails closed to untainted.
#[test]
fn encryption_out_of_range_projection_fails_closed() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let payload = (m.to_vec(), cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?);
            Ok(payload.7)
        }"#,
    ));
}

/// The removal probe is tri-state: a successful crypto-free rebuild is `Green` (the codec
/// never needed the crate), a missing-crate compile error is expected supplementary
/// evidence, and every other failure is `Inconclusive`.
#[test]
fn removal_probe_classifies_crypto_free_green() {
    let o = crate::process::CmdOutcome {
        status: Some(0),
        timed_out: false,
        stdout: b"test result: ok".to_vec(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    };
    assert_eq!(classify_probe(&o), RemovalProbe::Green);
}

#[test]
fn removal_probe_classifies_expected_missing_crate_failure() {
    let o = crate::process::CmdOutcome {
        status: Some(101),
        timed_out: false,
        stdout: Vec::new(),
        stderr: b"error[E0432]: unresolved import `aes_gcm`".to_vec(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    };
    assert_eq!(
        classify_probe(&o),
        RemovalProbe::ExpectedCryptoCompileFailure
    );
}

#[test]
fn removal_probe_classifies_unrelated_compile_failure_inconclusive() {
    let o = crate::process::CmdOutcome {
        status: Some(101),
        timed_out: false,
        stdout: Vec::new(),
        stderr: b"error: expected `;`, found `x`".to_vec(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    };
    assert_eq!(classify_probe(&o), RemovalProbe::Inconclusive);
}

#[test]
fn removal_probe_classifies_timeout_inconclusive() {
    let o = crate::process::CmdOutcome {
        status: None,
        timed_out: true,
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    };
    assert_eq!(classify_probe(&o), RemovalProbe::Inconclusive);
}

#[test]
fn removal_probe_accepts_only_expected_missing_crypto_failure() {
    let expected = RemovalProbe::ExpectedCryptoCompileFailure;
    assert!(removal_probe_proves_crypto(expected));
    assert!(consumer_probe_proves_crypto(true, expected));
    assert!(!consumer_probe_proves_crypto(false, expected));
    for rejected in [RemovalProbe::Green, RemovalProbe::Inconclusive] {
        assert!(!removal_probe_proves_crypto(rejected));
        assert!(!consumer_probe_proves_crypto(true, rejected));
    }

    let unrelated = crate::process::CmdOutcome {
        status: Some(101),
        timed_out: false,
        stdout: Vec::new(),
        stderr: b"error: unrelated compiler failure".to_vec(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    };
    let unrelated_probe = classify_probe(&unrelated);
    assert!(!removal_probe_proves_crypto(unrelated_probe));
    assert!(!consumer_probe_proves_crypto(true, unrelated_probe));

    let timeout = crate::process::CmdOutcome {
        timed_out: true,
        ..unrelated
    };
    let timeout_probe = classify_probe(&timeout);
    assert!(!removal_probe_proves_crypto(timeout_probe));
    assert!(!consumer_probe_proves_crypto(true, timeout_probe));
}

/// A malformed manifest makes the probe inconclusive before any command runs; that
/// setup failure is never treated as proof either way, and the crate still fails the AST
/// evidence (no parseable manifest, no exported flow) and the removal check.
#[test]
fn removal_probe_malformed_manifest_is_inconclusive() {
    let d = tempfile::tempdir().unwrap();

    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package\nname = 'unterminated",
    )
    .unwrap();
    let base = d.path().join("prune");
    assert_eq!(
        probe_crypto_removal(&base, &cap(d.path()), "filecrypt"),
        RemovalProbe::Inconclusive
    );
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a malformed manifest never gives AST evidence"
    );
    let checks = hidden_grader_checks("encryption");
    assert!(
        !checks.iter().all(|(_, c)| c(&cap(d.path()))),
        "a malformed manifest must not let the crate grade green (no proof from the failure)"
    );
}

#[test]
fn removal_probe_spawn_failure_fails_both_hidden_acceptance_paths() {
    let base = crate::harness::fresh_private_dir("llxprt-rs-spawn-probe").unwrap();
    let probe = run_removal_command("/definitely/missing/llxprt-cargo", &base);
    let _ = std::fs::remove_dir_all(base);
    assert_eq!(probe, RemovalProbe::Inconclusive);
    assert!(!removal_probe_proves_crypto(probe));
    assert!(!consumer_probe_proves_crypto(true, probe));
}
/// The genuine fixture still passes the stricter returned-flow evidence, proving the
/// value-sensitive scan stayed permissive enough for real AES-GCM crypto.
#[test]
fn encryption_green_fixture_flows_both_directions() {
    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    let roots: HashSet<String> = std::iter::once("aes_gcm".to_string()).collect();
    let ev = build_crate_evidence(&cap(d.path()), &roots).expect("fixture parses");
    let exports_both_operations = ev.is_exported("encrypt") && ev.is_exported("decrypt");
    assert!(exports_both_operations);
    assert!(
        exported_op_flows_to_return("encrypt", OpDir::Encrypt, &ev),
        "fixture encrypt result must flow into its return"
    );
    assert!(
        exported_op_flows_to_return("decrypt", OpDir::Decrypt, &ev),
        "fixture decrypt result must flow into its return"
    );
}

#[test]
fn encryption_grader_rejects_nondefault_library_targets() {
    for manifest_override in [
        "\n[lib]\npath = \"src/actual.rs\"\n",
        "\n[package]\nautolib = false\n",
    ] {
        let d = tempfile::tempdir().unwrap();
        write_encryption_good(d.path());
        let manifest = std::fs::read_to_string(d.path().join("Cargo.toml")).unwrap();
        let manifest = if manifest_override.contains("autolib") {
            manifest.replace("[package]", "[package]\nautolib = false")
        } else {
            format!("{manifest}{manifest_override}")
        };
        std::fs::write(d.path().join("Cargo.toml"), manifest).unwrap();
        assert!(!encryption_crate_grader(&cap(d.path())));
    }
}

#[test]
fn encryption_grader_rejects_incomplete_source_collection() {
    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    std::fs::write(d.path().join("src/invalid.rs"), [0xff, 0xfe]).unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));

    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    for index in 0..SRC_MAX_FILES {
        std::fs::write(d.path().join(format!("src/extra-{index}.rs")), "").unwrap();
    }
    assert!(!encryption_crate_grader(&cap(d.path())));

    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    let mut nested = d.path().join("src");
    for _ in 0..=SRC_MAX_DEPTH {
        nested.push("nested");
        std::fs::create_dir(&nested).unwrap();
    }
    assert!(!encryption_crate_grader(&cap(d.path())));

    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    std::fs::write(d.path().join("linked.rs"), "").unwrap();
    std::os::unix::fs::symlink(d.path().join("linked.rs"), d.path().join("src/linked.rs")).unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));
}

#[test]
fn encryption_mixed_collection_does_not_inherit_operation_flow() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let sealed = cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?;
            let mut out = Vec::new();
            out.extend_from_slice(m);
            out.extend_from_slice(&sealed);
            Ok(out)
        }"#,
    ));
}

#[test]
fn decryption_mixed_collection_does_not_inherit_operation_flow() {
    assert!(!direction_flow_fixture(
        r#"pub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let mut out = cipher.decrypt(&Nonce::default(), c).map_err(|_| "e".to_string())?;
            out.extend_from_slice(c);
            Ok(out)
        }"#,
        "decrypt",
        OpDir::Decrypt,
    ));
}

#[test]
fn value_replacing_combinators_do_not_preserve_encryption_flow() {
    for combinator in ["map", "and_then", "or_else"] {
        let function = format!(
            r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {{
                let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
                cipher.encrypt(&Nonce::default(), m).{combinator}(|_| Ok(m.to_vec()))
            }}"#
        );
        assert!(
            !direction_flow_fixture(&function, "encrypt", OpDir::Encrypt),
            "{combinator} must not preserve encryption provenance"
        );
    }
}

#[test]
fn value_replacing_combinators_do_not_preserve_decryption_flow() {
    for combinator in ["map", "and_then", "or_else"] {
        let function = format!(
            r#"pub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> {{
                let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
                cipher.decrypt(&Nonce::default(), c).{combinator}(|_| Ok(c.to_vec()))
            }}"#
        );
        assert!(
            !direction_flow_fixture(&function, "decrypt", OpDir::Decrypt),
            "{combinator} must not preserve decryption provenance"
        );
    }
}

#[test]
fn empty_collection_injected_with_operation_result_keeps_flow() {
    assert!(flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let sealed = cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())?;
            let mut out = Vec::new();
            out.extend_from_slice(&sealed);
            Ok(out)
        }"#,
    ));
}

/// A local extension trait can attach operation-shaped method names to a real crypto type.
/// The receiver type alone cannot establish that the external authenticated API owns the call.
#[test]
fn local_extension_trait_on_crypto_type_is_not_operation_evidence() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use aes_gcm::{Aes256Gcm, Key, KeyInit};\ntrait FakeCodec { fn encrypt(&self, data: &[u8]) -> Vec<u8>; fn decrypt(&self, data: &[u8]) -> Vec<u8>; }\nimpl FakeCodec for Aes256Gcm {\n fn encrypt(&self, data: &[u8]) -> Vec<u8> { data.iter().map(|b| b ^ 0x5a).collect() }\n fn decrypt(&self, data: &[u8]) -> Vec<u8> { data.iter().map(|b| b ^ 0x5a).collect() }\n}\npub fn encrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {\n let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default()); Ok(cipher.encrypt(data))\n}\npub fn decrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {\n let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default()); Ok(cipher.decrypt(data))\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a local extension trait must not turn custom methods into authenticated operations"
    );
}

/// Function-style calls cannot prove operation ownership because a local module can shadow an
/// accepted external crate root while an unrelated absolute path still references that crate.
#[test]
fn local_crypto_module_functions_are_not_operation_evidence() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "mod aes_gcm { pub fn encrypt(data: &[u8]) -> Vec<u8> { data.iter().map(|b| b ^ 0x5a).collect() } pub fn decrypt(data: &[u8]) -> Vec<u8> { data.iter().map(|b| b ^ 0x5a).collect() } }\nuse ::aes_gcm::Aes256Gcm as ExternalCipher;\npub fn external_marker() -> &'static str { std::any::type_name::<ExternalCipher>() }\npub fn encrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> { Ok(aes_gcm::encrypt(data)) }\npub fn decrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> { Ok(aes_gcm::decrypt(data)) }\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "local functions under a crypto-named module must not count as external operations"
    );
}

#[test]
fn cfg_disabled_decoys_and_macro_generated_api_fail_all_graders() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        r#"use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
#[cfg(any())]
pub fn encrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
    cipher.encrypt(&Nonce::default(), data).map_err(|_| "e".to_string())
}
#[cfg(any())]
pub fn decrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
    cipher.decrypt(&Nonce::default(), data).map_err(|_| "e".to_string())
}
macro_rules! insecure_api { () => {
    pub fn encrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.iter().map(|byte| byte ^ 0x5a).collect())
    }
    pub fn decrypt(_pw: &str, data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.iter().map(|byte| byte ^ 0x5a).collect())
    }
} }
insecure_api!();
"#,
    )
    .unwrap();
    let workspace = cap(d.path());
    assert!(!encryption_crate_grader(&workspace));
    assert!(!encryption_api_grader(&workspace));
    assert!(!encryption_consumer_grader(&workspace));
    assert!(!encryption_removal_grader(&workspace));
}

#[test]
fn function_local_crypto_type_shadow_is_not_operation_evidence() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            struct Aes256Gcm;
            impl Aes256Gcm {
                fn new<T>(_key: &T) -> Self { Self }
                fn encrypt<T>(&self, _nonce: &T, bytes: &[u8]) -> Result<Vec<u8>, String> {
                    Ok(bytes.to_vec())
                }
            }
            let cipher = Aes256Gcm::new(&());
            cipher.encrypt(&(), m)
        }"#,
    ));
}

fn flow_fixture(function: &str) -> bool {
    direction_flow_fixture(function, "encrypt", OpDir::Encrypt)
}

fn direction_flow_fixture(function: &str, exported: &str, direction: OpDir) -> bool {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname = \"flow-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        format!("use aes_gcm::{{aead::{{Aead, KeyInit}}, Aes256Gcm, Key, Nonce}};\n{function}\n"),
    )
    .unwrap();
    let roots: HashSet<String> = std::iter::once("aes_gcm".to_string()).collect();
    let ev = build_crate_evidence(&cap(d.path()), &roots).expect("fixture parses");
    exported_op_flows_to_return(exported, direction, &ev)
}

#[test]
fn encryption_dynamic_conditional_must_not_bless_custom_branch() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            if m.len() == usize::MAX {
                cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())
            } else {
                Ok(m.iter().map(|b| b ^ 7).collect())
            }
        }"#,
    ));
}

#[test]
fn encryption_ambiguous_conditional_requires_both_results() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            let real = cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string());
            if p.is_empty() { real } else { Ok(m.to_vec()) }
        }"#,
    ));
}

#[test]
fn encryption_ambiguous_match_requires_every_arm() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            match p.len() {
                0 => cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string()),
                _ => Ok(m.to_vec()),
            }
        }"#,
    ));
}

#[test]
fn encryption_guarded_match_fails_closed() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            match p.len() {
                n if n > 0 => cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string()),
                _ => cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string()),
            }
        }"#,
    ));
}

#[test]
fn encryption_untainted_early_success_return_fails() {
    assert!(!flow_fixture(
        r#"pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            if m.is_empty() { return Ok(Vec::new()); }
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string())
        }"#,
    ));
}

#[test]
fn encryption_unresolved_wrapper_does_not_propagate_operation_result() {
    assert!(!flow_fixture(
        r#"fn identity<T>(value: T) -> T { value }
        pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> {
            let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::default());
            identity(cipher.encrypt(&Nonce::default(), m).map_err(|_| "e".to_string()))
        }"#,
    ));
}

#[test]
fn inventory_reports_truncation_metadata() {
    let d = tempfile::tempdir().unwrap();
    write_pong_good(d.path());
    let inv = crate::harness::inventory(d.path());
    assert!(inv.files.contains(&"pong_logic.py".to_string()));
    assert!(!inv.files.contains(&"../outside".to_string()));
    assert!(!inv.truncated);
    let _ = PathBuf::new();
}
