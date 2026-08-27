use serde_json::{Map, Value};

const TOP_LEVEL_KEYS: &[&str] = &[
    "version",
    "provider",
    "model",
    "modelParams",
    "ephemeralSettings",
    "name",
    "_note",
    "type",
];

pub(super) const LOAD_BALANCER_UNSUPPORTED_MESSAGE: &str =
    "load-balancer profiles are not supported";
pub(super) const TOP_LEVEL_AUTH_UNSUPPORTED_MESSAGE: &str =
    "top-level 'auth' credential policy is not supported";

pub(super) fn validate_top_level(
    object: &Map<String, Value>,
    profile_name: &str,
) -> Result<(), String> {
    if object.contains_key("auth") {
        return Err(TOP_LEVEL_AUTH_UNSUPPORTED_MESSAGE.to_string());
    }
    if object.contains_key("policy")
        || object.contains_key("profiles")
        || object.contains_key("contextLimit")
        || object.contains_key("loadBalancer")
    {
        return Err(LOAD_BALANCER_UNSUPPORTED_MESSAGE.to_string());
    }

    if let Some(version) = object.get("version") {
        if version.as_u64() != Some(1) {
            return Err(format!(
                "profile {profile_name:?}: 'version' must be the integer 1"
            ));
        }
    }

    if let Some(profile_type) = object.get("type") {
        match profile_type.as_str() {
            Some("standard") => {}
            Some("loadbalancer") => {
                return Err(LOAD_BALANCER_UNSUPPORTED_MESSAGE.to_string());
            }
            _ => {
                return Err(format!(
                    "profile {profile_name:?}: 'type' must be exactly 'standard'"
                ));
            }
        }
    }

    for key in ["name", "_note"] {
        if let Some(value) = object.get(key) {
            let text = value
                .as_str()
                .ok_or_else(|| format!("profile {profile_name:?}: '{key}' must be a string"))?;
            if text.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(format!(
                    "profile {profile_name:?}: '{key}' exceeds the metadata size limit"
                ));
            }
        }
    }

    if let Some(key) = object.keys().find(|key| {
        !TOP_LEVEL_KEYS.contains(&key.as_str())
            && !matches!(
                key.as_str(),
                "auth" | "policy" | "profiles" | "contextLimit" | "loadBalancer"
            )
    }) {
        return Err(format!(
            "profile {profile_name:?}: unsupported top-level setting '{key}'"
        ));
    }

    Ok(())
}
