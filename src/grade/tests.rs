use super::*;
use crate::harness::test_result;
use std::path::PathBuf;

fn cap(path: &Path) -> crate::tools::WorkspaceCap {
    crate::tools::WorkspaceCap::open(path).expect("open test workspace")
}

fn ok_n() -> Vec<crate::harness::BbResult> {
    vec![test_result(true)]
}

fn build_good_starter(ws: &Path) {
    std::fs::write(
        ws.join("math_utils.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("test_math_utils.py"),
        "from math_utils import add\nassert add(2, 3) == 5\nprint('ok')\n",
    )
    .unwrap();
}

#[test]
fn verification_shell_does_not_load_login_startup_files() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".bash_profile"), "exit 97\n").unwrap();
    let (passed, output) = run_verification(
        &cap(workspace.path()),
        "printf verified",
        vec![(
            "HOME".to_string(),
            home.path().to_string_lossy().into_owned(),
        )],
    );
    assert!(passed, "verification failed: {output}");
    assert_eq!(output, "verified");
}

#[test]
fn green_starter_passes_all_categories() {
    let d = tempfile::tempdir().unwrap();
    build_good_starter(d.path());
    let ev = evidence("starter", d.path(), &ok_n());
    assert!(ev.protocol_pass);
    assert!(ev.tool_use_pass);
    assert!(ev.build_test_pass);
    assert!(ev.structural_pass);
    assert!(
        ev.hidden_graders_pass,
        "hidden graders must pass for a good starter"
    );
    assert!(ev.passed);
}

#[test]
fn missing_build_fails_even_with_good_protocol() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("math_utils.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("test_math_utils.py"),
        "from math_utils import add\nassert add(2, 3) == 99\nprint('NOPE')\n",
    )
    .unwrap();
    let ev = evidence("starter", d.path(), &ok_n());
    assert!(ev.protocol_pass);
    assert!(!ev.build_test_pass, "the python check must fail");
    assert!(!ev.passed);
}

#[test]
fn failed_protocol_fails_even_with_good_build() {
    let d = tempfile::tempdir().unwrap();
    build_good_starter(d.path());
    let failed = vec![test_result(false)];
    let ev = evidence("starter", d.path(), &failed);
    assert!(!ev.protocol_pass);
    assert!(!ev.passed);
}

#[test]
fn hidden_grader_checks_are_real_per_file() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("math_utils.py"), "def add(a, b):\n    pass\n").unwrap();
    let checks = hidden_grader_checks("starter");
    assert!(checks.iter().any(|(_, c)| !c(&cap(d.path()))));
}

/// Hidden graders never read a symlinked file and cap oversized files.
#[test]
fn hidden_grader_reads_reject_symlinks_and_cap_size() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("real.txt"), "def add(a, b): return 3").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("real.txt"), d.path().join("link.txt")).unwrap();
    assert!(grader_file(&cap(d.path()), "link.txt").is_none());
    std::fs::write(d.path().join("big.txt"), "x".repeat(GRADER_MAX_BYTES + 1)).unwrap();
    assert!(grader_file(&cap(d.path()), "big.txt").is_none());
}

#[test]
fn verify_depth_is_empty_for_unknown_scenario() {
    assert!(verify_commands("nope").is_empty());
    assert!(required_files("nope").is_empty());
    assert!(hidden_grader_checks("nope").is_empty());
}

/// A valid Pong workspace matching the stable contract passes full score: the
/// grader-authored probe (not the artifact's test) imports `pong_logic` and asserts
/// the exact behavior.
fn write_pong_good(ws: &Path) {
    std::fs::write(
        ws.join("pong_logic.py"),
        "FIELD_W = 800\nFIELD_H = 600\nPADDLE_H = 80\n\ndef move_ball(ball, vel):\n    return (ball[0] + vel[0], ball[1] + vel[1])\n\ndef bounce(vel, axis):\n    v = [vel[0], vel[1]]\n    v[axis] = -v[axis]\n    return (v[0], v[1])\n\ndef move_paddle(paddle, dy):\n    return max(0, min(FIELD_H - PADDLE_H, paddle + dy))\n\ndef point_scored(ball):\n    return ball[0] < 0 or ball[0] > FIELD_W\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("test_pong.py"),
        "import pong_logic\nassert pong_logic.move_ball((100, 50), (3, -2)) == (103, 48)\nassert pong_logic.bounce((3, 4), 0) == (-3, 4)\nassert pong_logic.move_paddle(0, -5) == 0\nassert pong_logic.point_scored((-1, 50)) is True\nassert pong_logic.point_scored((50, 50)) is False\nprint('PASS')\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("pong.py"),
        "import pong_logic\nprint('PONG', pong_logic.move_ball((1, 2), (1, 1)))\n",
    )
    .unwrap();
}

#[test]
fn pong_green_full_score_passes() {
    let d = tempfile::tempdir().unwrap();
    write_pong_good(d.path());
    let ev = evidence("pong", d.path(), &ok_n());
    assert!(ev.structural_pass);
    assert!(
        ev.build_test_pass,
        "python3 test_pong.py must pass for a good core+test"
    );
    assert!(ev.hidden_graders_pass);
}

/// A Pong identity stub (move_ball/bounce/move_paddle return their inputs, and
/// point_scored always returns False) must fail the **grader-authored** behavior
/// probe even though its own test file passes: an identity stub is superficially
/// structured but never exhibits the Pong contract.
#[test]
fn pong_identity_stub_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("pong_logic.py"),
        "FIELD_W = 800
FIELD_H = 600
PADDLE_H = 80

def move_ball(ball, vel):
    return (ball[0], ball[1])

def bounce(vel, axis):
    return (vel[0], vel[1])

def move_paddle(paddle, dy):
    return paddle

def point_scored(ball):
    return False
",
    )
    .unwrap();
    std::fs::write(
        d.path().join("test_pong.py"),
        "import pong_logic
assert True
",
    )
    .unwrap();
    std::fs::write(d.path().join("pong.py"), "import pong_logic").unwrap();
    // The verification command passes (the artifact's own test is trivial)…
    let (ok, _) = try_verify(&cap(d.path()), "python3 test_pong.py");
    assert!(
        ok,
        "the identity stub's own test passes: superficially valid"
    );
    // …but the grader-authored behavior probe must reject it, so no identity Pong
    // can ever grade green.
    assert!(!pong_probe_grader(&cap(d.path())));
    let checks = hidden_grader_checks("pong");
    assert!(!checks.iter().all(|(_, c)| c(&cap(d.path()))));
    let ev = evidence("pong", d.path(), &ok_n());
    assert!(!ev.passed);
}

/// A fully correct Flappy workspace (gravity, flap, real collision, real scoring)
/// must pass every hidden grader through the **grader-authored** behavior probe, not
/// a source-string check or the artifact's own test.
fn write_flappy_good(ws: &Path) {
    std::fs::write(
        ws.join("flappy_logic.py"),
        "GRAV = 1.0\nFLAP_VY = -8.0\nBIRD_R = 8.0\nPIPE_W = 60.0\n\ndef update_bird(b):\n    x, y, vy = b\n    return (x, y + vy, vy + GRAV)\n\ndef flap(b):\n    x, y, vy = b\n    return (x, y, FLAP_VY)\n\ndef collides(bird, pipes):\n    x, y, vy = bird\n    return any(abs(px - x) < PIPE_W / 2 + BIRD_R and ((y - BIRD_R) < top or (y + BIRD_R) > bottom) for (px, top, bottom) in pipes)\n\ndef passed(bird, pipe):\n    return bird[0] > pipe[0] + PIPE_W / 2\n\ndef score(bird, pipes):\n    return sum(1 for p in pipes if passed(bird, p))\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("test_flappy.py"),
        "import flappy_logic\nassert True\nprint('PASS')\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("flappy.py"),
        "import flappy_logic\nprint('FLAPPY', flappy_logic.score((500, 200, 0), [(100, 50, 250)]))\n",
    )
    .unwrap();
}

#[test]
fn flappy_green_full_score_passes() {
    let d = tempfile::tempdir().unwrap();
    write_flappy_good(d.path());
    let ev = evidence("flappy", d.path(), &ok_n());
    assert!(ev.structural_pass);
    assert!(
        ev.build_test_pass,
        "python3 test_flappy.py must pass for a good core+test"
    );
    assert!(
        ev.hidden_graders_pass,
        "the grader-authored probe must pass for a genuine Flappy core"
    );
    assert!(ev.passed);
}

/// An identity Flappy (no gravity, no flap, collides always False) must fail.
#[test]
fn flappy_identity_stub_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("flappy_logic.py"),
        "import random\nGRAV = 1.0\nFLAP_VY = -8.0\nBIRD_R = 8.0\nPIPE_W = 60.0\n\ndef update_bird(b):\n    x, y, vy = b\n    return (x, y, vy)\n\ndef flap(b):\n    x, y, vy = b\n    return (x, y, vy)\n\ndef collides(bird, pipes):\n    return False\n\ndef passed(bird, pipe):\n    return False\n\ndef score(bird, pipes):\n    return 0\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("test_flappy.py"),
        "import flappy_logic\nassert True\n",
    )
    .unwrap();
    std::fs::write(d.path().join("flappy.py"), "import flappy_logic").unwrap();
    assert!(!flappy_probe_grader(&cap(d.path())));
    let checks = hidden_grader_checks("flappy");
    assert!(!checks.iter().all(|(_, c)| c(&cap(d.path()))));
    let ev = evidence("flappy", d.path(), &ok_n());
    assert!(!ev.passed);
}

fn write_encryption_good(ws: &Path) {
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]
name = \"filecrypt\"
version = \"0.1.0\"
edition = \"2021\"

[dependencies]
aes-gcm = \"0.10\"
",
    )
    .unwrap();
    // Route cargo artifacts to a shared offline cache so the graded `cargo test
    // --offline` reuses already compiled deps instead of recompiling per test.
    std::fs::create_dir_all(ws.join(".cargo")).unwrap();
    std::fs::write(
        ws.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"/tmp/llxprt-rs-grader-enc-target\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(
        ws.join("src/lib.rs"),
        "use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};\nuse aes_gcm::{Aes256Gcm, Key};\n\nfn key_from_password(password: &str) -> Key<Aes256Gcm> {\n    let mut key = Key::<Aes256Gcm>::default();\n    let n = password.len().min(key.len());\n    key[..n].copy_from_slice(&password.as_bytes()[..n]);\n    key\n}\n\npub fn encrypt(password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {\n    let cipher = Aes256Gcm::new(&key_from_password(password));\n    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);\n    let mut out = nonce.to_vec();\n    let mut sealed = cipher.encrypt(&nonce, plaintext).map_err(|_| \"encrypt failure\".to_string())?;\n    out.append(&mut sealed);\n    Ok(out)\n}\n\npub fn decrypt(password: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {\n    if ciphertext.len() < 12 { return Err(\"ciphertext too short\".to_string()); }\n    let cipher = Aes256Gcm::new(&key_from_password(password));\n    let nonce = aes_gcm::Nonce::from_slice(&ciphertext[..12]);\n    cipher.decrypt(nonce, &ciphertext[12..]).map_err(|_| \"decrypt failure\".to_string())\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("tests")).unwrap();
    std::fs::write(
        ws.join("tests/roundtrip.rs"),
        "use filecrypt::{decrypt, encrypt};\n\n#[test]\nfn roundtrip_works() {\n    let msg = b\"attack at dawn\";\n    let ct = encrypt(\"secret pass\", msg).unwrap();\n    assert_ne!(ct, msg);\n    assert_eq!(decrypt(\"secret pass\", &ct).unwrap(), msg);\n}\n\n#[test]\nfn wrong_password_fails() {\n    let ct = encrypt(\"right pass\", b\"data\").unwrap();\n    assert!(decrypt(\"wrong pass\", &ct).is_err());\n}\n",
    )
    .unwrap();
}

/// The synthetic encryption fixture is a genuinely correct crate: its `cargo test
/// --offline` builds green and every hidden grader (including the external consumer)
/// passes.
#[test]
fn encryption_green_full_score_passes() {
    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    assert!(
        encryption_crate_grader(&cap(d.path())),
        "green fixture must use its declared AEAD crate"
    );
    assert!(
        encryption_api_grader(&cap(d.path())),
        "green fixture must expose the encryption API"
    );
    assert!(
        encryption_consumer_grader(&cap(d.path())),
        "green fixture must pass the external encryption consumer"
    );
    assert!(
        encryption_removal_grader(&cap(d.path())),
        "green fixture must fail crypto removal (its crypto is real)"
    );
    let ev = evidence("encryption", d.path(), &ok_n());
    assert!(
        ev.structural_pass,
        "Cargo.lock must be materialized by cargo"
    );
    assert!(
        ev.build_test_pass,
        "cargo test --offline must pass for the genuine crate"
    );
    assert!(
        ev.hidden_graders_pass,
        "hidden graders must pass for the genuine crate"
    );
    assert!(ev.passed);
}

/// A "crypto" crate that lists aes-gcm only in a comment (so the toml parser finds
/// no dependency) and whose wrong-password decrypt succeeds (identity) must fail.
#[test]
fn encryption_identity_with_comment_only_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n# aes-gcm = \"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(_password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> { Ok(plaintext.to_vec()) }\npub fn decrypt(_password: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> { Ok(ciphertext.to_vec()) }\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("tests")).unwrap();
    std::fs::write(
        d.path().join("tests/roundtrip.rs"),
        "use filecrypt::encrypt;\n#[test]\nfn t() { let _ = encrypt(\"k\", &[7]); }",
    )
    .unwrap();
    // The crate dependency check must reject the comment-only "dependency".
    assert!(!encryption_crate_grader(&cap(d.path())));
    assert!(!encryption_consumer_grader(&cap(d.path())));
    let checks = hidden_grader_checks("encryption");
    assert!(
        !checks.iter().all(|(_, c)| c(&cap(d.path()))),
        "identity encryption must fail a hidden grader"
    );
}

/// The encryption dependency check parses `[dependencies]` keys from TOML, so a
/// real aes-gcm entry counts and a comment never does.
#[test]
fn encryption_dependency_keys_are_parsed_not_comments() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname = \"filecrypt\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naes-gcm = \"0.10\"\n\n[dev-dependencies]\n# chacha20poly1305 = \"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt() {}\npub fn decrypt() {}\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a declared but unused dependency must not count"
    );
}

#[test]
fn encryption_comment_and_string_usage_do_not_count() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "// aes_gcm::Aes256Gcm\nconst CLAIM: &str = \"aes_gcm\";\npub fn encrypt(_: &str, p: &[u8]) -> Result<Vec<u8>, String> { Ok(p.to_vec()) }\npub fn decrypt(_: &str, p: &[u8]) -> Result<Vec<u8>, String> { Ok(p.to_vec()) }\n",
    )
    .unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));
}

#[test]
fn encryption_fixed_nonce_fails_external_consumer() {
    let d = tempfile::tempdir().unwrap();
    write_encryption_good(d.path());
    let path = d.path().join("src/lib.rs");
    let source = std::fs::read_to_string(&path).unwrap().replace(
        "let nonce = Aes256Gcm::generate_nonce(&mut OsRng);",
        "let nonce = *aes_gcm::Nonce::from_slice(&[0u8; 12]);",
    );
    std::fs::write(path, source).unwrap();
    assert!(encryption_crate_grader(&cap(d.path())));
    assert!(!encryption_consumer_grader(&cap(d.path())));
}

#[test]
fn encryption_missing_decrypt_fails_hidden() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(k: &str, p: &[u8]) -> Result<Vec<u8>, String> { Ok(p.to_vec()) }",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("tests")).unwrap();
    std::fs::write(
        d.path().join("tests/roundtrip.rs"),
        "use filecrypt::encrypt;\n#[test]\nfn t() { let _ = encrypt(\"k\", &[7]); }",
    )
    .unwrap();
    assert!(!encryption_api_grader(&cap(d.path())));
    let checks = hidden_grader_checks("encryption");
    assert!(!checks.iter().all(|(_, c)| c(&cap(d.path()))));
}

/// A declared but never-used established dependency yields no same-crate crypto
/// evidence: an imported-but-dormant crate is not an encryption implementation.
#[test]
fn encryption_rejects_unused_dependency() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(m.to_vec()) }\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\n",
    )
    .unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));
}

/// A `type_name` marker referencing the crate (or the crate used only by name in a
/// string, comment, or bare import) is never crypto evidence.
#[test]
fn encryption_rejects_type_name_marker() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(p: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let _n = std::any::type_name::<aes_gcm::Aes256Gcm>();\n    Ok(m.to_vec())\n}\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a type_name marker is not a receiver operation"
    );
}

/// Real crypto reached only from a **dead** helper the exported functions never call
/// cannot satisfy the evidence.
#[test]
fn encryption_dead_helper_never_counts() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};\npub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(m.to_vec()) }\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\nfn dead_seal(m: &[u8]) -> Vec<u8> { let key = Key::<Aes256Gcm>::default(); let c = Aes256Gcm::new(&key); let n = Nonce::default(); let _ = c.encrypt(&n, m); m.to_vec() }\n",
    )
    .unwrap();
    assert!(
        !encryption_crate_grader(&cap(d.path())),
        "a dead (unreached) crypto helper must not pass"
    );
}

/// A `type_name` marker inside `encrypt` plus a custom XOR/checksum scheme still has
/// no recognized crypto-derived receiver operation: the evidence must reject it.
#[test]
fn encryption_type_name_plus_custom_xor_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "fn sum(pw: &[u8], m: &[u8]) -> u8 { pw.iter().chain(m).fold(0u8, |a, b| a.wrapping_add(*b)) }\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let _t = std::any::type_name::<aes_gcm::Aes256Gcm>();\n    let mut o = Vec::new();\n    for (i, b) in m.iter().enumerate() { o.push(b.wrapping_add(pw.as_bytes()[i % pw.len().max(1)])); }\n    o.push(sum(pw.as_bytes(), m));\n    Ok(o)\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    let (body, tag) = c.split_at(c.len().saturating_sub(1));\n    let m: Vec<u8> = body.iter().enumerate().map(|(i, b)| b.wrapping_sub(pw.as_bytes()[i % pw.len().max(1)])).collect();\n    if sum(pw.as_bytes(), &m) != tag[0] { return Err(\"bad\".to_string()); }\n    Ok(m)\n}\n",
    )
    .unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));
}

/// A locally-defined receiver with its own `seal`/`reverse` methods is never
/// crypto-derived: fake local methods cannot fabricate the call graph.
#[test]
fn encryption_fake_local_receiver_methods_fail() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "struct MyBox;\nimpl MyBox {\n    fn seal(&self, p: &[u8]) -> Vec<u8> { let mut o = p.to_vec(); o.reverse(); o }\n}\npub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(MyBox.seal(m)) }\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { let mut o = c.to_vec(); o.reverse(); Ok(o) }\n",
    )
    .unwrap();
    assert!(!encryption_crate_grader(&cap(d.path())));
}

/// An established crate aliased to a local `path` copy is a local path dependency:
/// vendoring a fake under the allow-listed name cannot bypass the established-only rule.
#[test]
fn encryption_alias_local_path_bypass_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\ncrypto = { package = \"aes-gcm\", path = \"vendor/aesgcm\" }\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(m.to_vec()) }\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\n",
    )
    .unwrap();
    assert!(manifest_has_path_dep(&cap(d.path())));
    assert!(!encryption_crate_grader(&cap(d.path())));
}

/// A registry alias (`renamed = { version, package }`) resolves to the established
/// package with real alias support and is never mistaken for a path dependency.
#[test]
fn encryption_registry_alias_is_established() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\ncrypto = { version = \"0.10\", package = \"aes-gcm\" }\n\n[dev-dependencies]\ncha = { version = \"0.12\", package = \"chacha20poly1305\" }\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn encrypt(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(m.to_vec()) }\npub fn decrypt(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\n",
    )
    .unwrap();
    let packages = established_registry_packages(&cap(d.path()));
    assert!(packages.iter().any(|p| p == "aes-gcm"));
    assert!(packages.iter().any(|p| p == "chacha20poly1305"));
    assert!(!manifest_has_path_dep(&cap(d.path())));
}

/// A crate whose exported `encrypt`/`decrypt` are re-export aliases to differently
/// named sibling functions (rather than `pub fn` items) has no stable export: the API
/// and evidence checks fail closed instead of trusting a thin re-export.
#[test]
fn encryption_missing_sibling_copy_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "mod sibling;\npub use sibling::fake_enc as encrypt;\npub use sibling::fake_dec as decrypt;\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("src/sibling.rs"),
        "pub fn fake_enc(_p: &str, m: &[u8]) -> Result<Vec<u8>, String> { Ok(m.to_vec()) }\npub fn fake_dec(_p: &str, c: &[u8]) -> Result<Vec<u8>, String> { Ok(c.to_vec()) }\n",
    )
    .unwrap();
    assert!(
        !encryption_api_grader(&cap(d.path())),
        "re-export aliases are not the exported fns"
    );
    assert!(!encryption_crate_grader(&cap(d.path())));
}

/// A self-consistent crypto-free fake (whose behaviors all look right because they are
/// implemented locally) is rejected by the crypto-removal probe: strip every
/// acknowledged crypto dependency and it still builds and passes the behavioral tests.
#[test]
fn encryption_removal_probe_rejects_crypto_free_fake() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"filecrypt\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\naes-gcm=\"0.10\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "use std::sync::atomic::{AtomicU64, Ordering};\nstatic NONCE: AtomicU64 = AtomicU64::new(1);\nfn fold(pw: &[u8], d: &[u8]) -> u8 { pw.iter().chain(d).fold(0u8, |a, b| a.wrapping_add(*b)) }\npub fn encrypt(pw: &str, m: &[u8]) -> Result<Vec<u8>, String> {\n    let n = NONCE.fetch_add(1, Ordering::Relaxed);\n    let salt: Vec<u8> = (0..24).map(|k| ((n).wrapping_mul(k as u64 + 1).wrapping_mul(2654435761) >> (k % 8)) as u8).collect();\n    let mut ct = salt.clone();\n    let x: Vec<u8> = m.iter().enumerate().map(|(i, b)| b ^ pw.as_bytes()[i % pw.as_bytes().len().max(1)] ^ salt[i % 24]).collect();\n    ct.extend(&x);\n    ct.push(fold(pw.as_bytes(), &salt));\n    ct.push(fold(pw.as_bytes(), &x));\n    Ok(ct)\n}\npub fn decrypt(pw: &str, c: &[u8]) -> Result<Vec<u8>, String> {\n    if c.len() < 26 { return Err(\"tiny\".to_string()); }\n    let salt = &c[..24];\n    let x = &c[24..c.len() - 2];\n    if c[c.len() - 2] != fold(pw.as_bytes(), salt) { return Err(\"bad\".to_string()); }\n    if c[c.len() - 1] != fold(pw.as_bytes(), x) { return Err(\"bad\".to_string()); }\n    let m: Vec<u8> = x.iter().enumerate().map(|(i, b)| b ^ pw.as_bytes()[i % pw.as_bytes().len().max(1)] ^ salt[i % 24]).collect();\n    Ok(m)\n}\n",
    )
    .unwrap();
    assert!(
        !encryption_removal_grader(&cap(d.path())),
        "a crypto-free fake cannot survive the removal probe reject"
    );
    let checks = hidden_grader_checks("encryption");
    assert!(!checks.iter().all(|(_, c)| c(&cap(d.path()))));
}

/// A symlinked required file is never counted as present.
#[test]
fn symlinked_required_file_is_not_present() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let other = outside.path().join("other.txt");
    std::fs::write(&other, "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&other, d.path().join("needed.txt")).unwrap();
    assert_eq!(
        crate::harness::score_present(d.path(), &["needed.txt"]),
        0.0
    );
}
mod flow;

#[test]
fn retained_workspace_capability_survives_path_replacement_before_grading() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let moved = root.path().join("moved");
    std::fs::create_dir(&workspace).unwrap();
    build_good_starter(&workspace);
    let workspace_cap = cap(&workspace);

    std::fs::rename(&workspace, &moved).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("math_utils.py"),
        "raise RuntimeError('attacker')\n",
    )
    .unwrap();

    let evidence = evidence_with_cap("starter", &workspace_cap, &ok_n());
    assert!(evidence.passed);
    assert!(evidence
        .inventory
        .files
        .iter()
        .any(|path| path == "test_math_utils.py"));
    let report = report_with_cap("starter", &workspace, &workspace_cap, &ok_n());
    assert_eq!(report["question"]["passed"], true);
}

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

#[test]
#[cfg(unix)]
fn grader_readers_reject_fifos_without_blocking() {
    let workspace = tempfile::tempdir().unwrap();
    make_fifo(&workspace.path().join("artifact"));
    let workspace_cap = cap(workspace.path());

    assert!(basic::read_ws_bytes(&workspace_cap, "artifact").is_none());
    assert!(open_read_capped(workspace_cap.root_dir(), std::ffi::OsStr::new("artifact")).is_none());
}

#[test]
#[cfg(unix)]
fn verification_cannot_block_hidden_grading_by_substituting_a_fifo() {
    let workspace = tempfile::tempdir().unwrap();
    write_pong_good(workspace.path());
    std::fs::write(
        workspace.path().join("test_pong.py"),
        "import os\nos.unlink('pong.py')\nos.mkfifo('pong.py')\nprint('PASS')\n",
    )
    .unwrap();

    let result = evidence("pong", workspace.path(), &ok_n());
    assert!(result.build_test_pass);
    assert!(!result.structural_pass);
    assert!(!result.hidden_graders_pass);
    assert!(!result.passed);
}
