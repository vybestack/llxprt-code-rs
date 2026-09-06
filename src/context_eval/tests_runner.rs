//! Tests for generated Rust adapter configuration.

use super::{manifest, runner};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from("evals/context-management/fixtures")
}

fn good_manifest() -> String {
    r#"
schema_version = 1
id = "selftest-probe"
owner_phase = 7
arm = "feature"
expected_status = "red"
expected_reason_class = "context-limit"

[profile]
name = "ctxeval-loopback"
provider = "openai"
model = "ctxeval-fixture"
context_limit_tokens = 20000
max_output_tokens = 2048

[stimulus]
prompt = "CTXEVAL-SELFTEST"

[runtime]
name = "profile-limit"
context_limit = 20000
"#
    .to_string()
}

#[test]
fn prepare_emits_typed_settings_and_profile_without_base_url() {
    let scen = manifest::parse_str(&good_manifest(), &fixtures()).unwrap();
    let base_url = "http://127.0.0.1:1/v1";
    let dir = std::env::temp_dir().join(format!("ctxeval-settings-{}", crate::harness::uniq()));
    let prepared = runner::prepare(&dir, &scen, base_url, Vec::new(), Vec::new()).unwrap();

    let settings = crate::settings::load_user_file(&prepared.config_home).unwrap();
    assert_eq!(settings.provider.base_url.as_deref(), Some(base_url));

    let profile: serde_json::Value = serde_json::from_slice(
        &fs::read(
            prepared
                .config_home
                .join("profiles")
                .join(format!("{}.json", scen.profile.name)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(profile["ephemeralSettings"].get("base-url").is_none());
    assert_eq!(profile["provider"], json!(scen.profile.provider));
    assert_eq!(profile["model"], json!(scen.profile.model));
    assert_eq!(
        profile["ephemeralSettings"]["context-limit"],
        json!(scen.runtime.context_limit)
    );
    fs::remove_dir_all(&dir).ok();
}
