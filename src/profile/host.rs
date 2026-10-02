//! Native host settings for non-Codex profiles. Codex retains its own strict
//! grammar and external-host numeric image policy from #309.
use super::EphemeralSettings;
use serde_json::Value;

pub(super) fn parse(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &Value,
    name: &str,
) -> Result<bool, String> {
    if key == "stream-first-response-timeout-ms" {
        settings.timeout_ms = match (value.as_i64(), value.as_u64()) {
            (Some(-1), _) | (_, Some(0)) => None,
            (_, Some(n)) => {
                crate::limits::validate_timeout(Some(std::time::Duration::from_millis(n)))?;
                Some(n)
            }
            _ => {
                return Err(format!(
                    "profile {name:?}: '{key}' must be -1 or a non-negative integer"
                ))
            }
        };
        return Ok(true);
    }
    if !matches!(
        key,
        "image-resize.maxLongEdge" | "image-resize.maxShortEdge" | "image-resize.maxPixels"
    ) {
        return Ok(false);
    }
    let n = value
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a positive integer"))?;
    if key != "image-resize.maxPixels" && n > u64::from(u32::MAX) {
        return Err(format!(
            "profile {name:?}: '{key}' exceeds image dimension range"
        ));
    }
    let number = Some(serde_json::Number::from(n));
    match key {
        "image-resize.maxLongEdge" => settings.host_image_resize.max_long_edge = number,
        "image-resize.maxShortEdge" => settings.host_image_resize.max_short_edge = number,
        "image-resize.maxPixels" => settings.host_image_resize.max_pixels = number,
        _ => unreachable!(),
    }
    Ok(true)
}

pub(super) fn validate(settings: &EphemeralSettings, name: &str) -> Result<(), String> {
    if let (Some(long), Some(short)) = (
        settings
            .host_image_resize
            .max_long_edge
            .as_ref()
            .and_then(serde_json::Number::as_u64),
        settings
            .host_image_resize
            .max_short_edge
            .as_ref()
            .and_then(serde_json::Number::as_u64),
    ) {
        if short > long {
            return Err(format!(
                "profile {name:?}: image short edge exceeds long edge"
            ));
        }
    }
    Ok(())
}
