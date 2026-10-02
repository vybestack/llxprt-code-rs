use clap::Parser as _;
use llxprt_code_rs::cli::Args;
use std::process::Command;

#[test]
fn localoauth_is_explicit_and_login_requires_it() {
    let default = Args::try_parse_from(["llxprt-code-rs"]).unwrap();
    assert!(!default.localoauth);
    assert!(!default.oauth_login);
    assert!(Args::try_parse_from(["llxprt-code-rs", "--oauth-login"]).is_err());
    let login = Args::try_parse_from(["llxprt-code-rs", "--localoauth", "--oauth-login"]).unwrap();
    assert!(login.localoauth && login.oauth_login);
    assert!(Args::try_parse_from([
        "llxprt-code-rs",
        "--localoauth",
        "--oauth-login",
        "--print-config"
    ])
    .is_err());
    assert!(Args::try_parse_from([
        "llxprt-code-rs",
        "--localoauth",
        "--oauth-login",
        "--prompt",
        "ignored"
    ])
    .is_err());
}

#[test]
fn missing_local_token_is_scrubbed_single_envelope_without_keychain_or_fallback() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let profile = home.join("codex.json");
    std::fs::write(
        &profile,
        include_str!("fixtures/task-profile/astra-headless.json"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .args([
            "--localoauth",
            "--session",
            "local-missing",
            "--prompt",
            "do not send",
        ])
        .arg("--profile-load")
        .arg(&profile)
        .env("LLXPRT_CONFIG_HOME", &home)
        .env_remove("LLXPRT_CONFIG_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["session_id"], "local-missing");
    assert!(stdout.contains("--localoauth --oauth-login"));
    assert!(!stdout.to_lowercase().contains("keychain"));
    assert!(!String::from_utf8(output.stderr)
        .unwrap()
        .to_lowercase()
        .contains("keychain"));
    assert!(!home.join("code-rs-sessions/local-missing").exists());
}

#[test]
fn print_config_with_localoauth_does_not_touch_any_credential_store() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let profile = home.join("codex.json");
    std::fs::write(
        &profile,
        include_str!("fixtures/task-profile/astra-headless.json"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .args(["--localoauth", "--print-config"])
        .arg("--profile-load")
        .arg(&profile)
        .env("LLXPRT_CONFIG_HOME", &home)
        .env_remove("LLXPRT_CONFIG_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert!(!home.join("oauth").exists());
}
