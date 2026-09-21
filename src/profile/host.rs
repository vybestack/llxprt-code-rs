//! Host policy shared by every provider grammar. No provider wire parameters.
use super::EphemeralSettings;
use serde_json::Value;

pub(super) const KEYS: &[&str] = &[
    "shell-default-timeout-seconds",
    "shell-max-timeout-seconds",
    "stream-first-response-timeout-ms",
    "image-resize.maxLongEdge",
    "image-resize.maxShortEdge",
    "image-resize.maxPixels",
];

pub(super) fn parse(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &Value,
    name: &str,
) -> Result<bool, String> {
    if !KEYS.contains(&key) {
        return Ok(false);
    }
    // -1 means no profile-imposed budget. The host's finite safety bounds remain.
    if value.as_i64() == Some(-1) {
        match key {
            "shell-max-timeout-seconds" => {
                settings.shell_max_timeout_seconds = Some(u32::MAX as u64);
                return Ok(true);
            }
            "stream-first-response-timeout-ms" => {
                settings.timeout_ms = None;
                return Ok(true);
            }
            _ => {}
        }
    }
    // Zero selects the native default budget, not an unbounded request.
    if key == "stream-first-response-timeout-ms" && value.as_u64() == Some(0) {
        settings.timeout_ms = None;
        return Ok(true);
    }
    let n = value
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a positive integer"))?;
    match key {
        "shell-default-timeout-seconds" | "shell-max-timeout-seconds" => {
            if n > u32::MAX as u64 {
                return Err(format!(
                    "profile {name:?}: '{key}' exceeds supported seconds"
                ));
            }
            if key == "shell-default-timeout-seconds" {
                settings.shell_default_timeout_seconds = Some(n);
            } else {
                settings.shell_max_timeout_seconds = Some(n);
            }
        }
        "stream-first-response-timeout-ms" => {
            // Native backends perform non-streaming requests: bound the entire response.
            crate::limits::validate_timeout(Some(std::time::Duration::from_millis(n)))?;
            settings.timeout_ms = Some(n);
        }
        "image-resize.maxLongEdge" | "image-resize.maxShortEdge" => {
            let n = u32::try_from(n)
                .map_err(|_| format!("profile {name:?}: '{key}' exceeds image dimension range"))?;
            if key == "image-resize.maxLongEdge" {
                settings.image_resize.max_long_edge = Some(n);
            } else {
                settings.image_resize.max_short_edge = Some(n);
            }
        }
        "image-resize.maxPixels" => settings.image_resize.max_pixels = Some(n),
        _ => unreachable!(),
    }
    Ok(true)
}

pub(super) fn validate(settings: &EphemeralSettings, name: &str) -> Result<(), String> {
    if settings.shell_default_timeout_seconds.unwrap_or(120)
        > settings.shell_max_timeout_seconds.unwrap_or(120)
    {
        return Err(format!(
            "profile {name:?}: shell default timeout exceeds shell maximum timeout"
        ));
    }
    if let (Some(long), Some(short)) = (
        settings.image_resize.max_long_edge,
        settings.image_resize.max_short_edge,
    ) {
        if short > long {
            return Err(format!(
                "profile {name:?}: image short edge exceeds long edge"
            ));
        }
    }
    Ok(())
}
