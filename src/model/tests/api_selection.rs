use crate::profile::parse_profile_value;
use serde_json::json;

/// The public entry point runs the API-kind gate itself, before it resolves the
/// config root: an unsupported selection is refused with the gate's own error, the
/// same one `from_profile_in` returns. Mutating `HOME`/`LLXPERT_CONFIG_HOME` here to
/// prove ordering would race the other tests in this binary (the process environment
/// is global and tests run in parallel), so the proof is that the gate's refusal --
/// which nothing else in `from_profile` produces -- comes back from the public path
/// at all (issue 73, H1).
#[test]
fn from_profile_refuses_unsupported_api_selection_before_config_root() {
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": { "apiMode": "responses" }
        }),
        "test",
    )
    .unwrap();
    let error = crate::model::ModelConfig::from_profile(&profile, false, false).unwrap_err();
    assert!(
        matches!(error, crate::model::ModelError::UnsupportedApiSelection(_)),
        "{error:?}"
    );
}
