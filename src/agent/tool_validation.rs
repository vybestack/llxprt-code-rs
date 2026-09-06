//! Tool-call validation shared by the normal and forced tool-round paths.

use super::*;

/// A tool call is valid only when its id is unique across the whole attempt
/// (`seen`) and the arguments are a JSON object. Unknown or disabled names are
/// returned as correctable refusals.
///
/// Tool names come from `crate::tools::known_tool` (the `TOOL_CATALOGUE`) via the
/// glob import above, so this path and the malformed-tool-call detector share one
/// source of truth.
pub(super) fn validate_calls(
    seen: &mut std::collections::HashSet<String>,
    result: &LlmResult,
    allow_shell: bool,
) -> Result<(Vec<ToolCall>, Vec<ToolCall>), String> {
    for c in &result.calls {
        if c.id.trim().is_empty() {
            return Err("model returned a tool call with an empty id".into());
        }
        if !seen.insert(c.id.clone()) {
            return Err(format!("duplicate tool call id {}", c.id));
        }
    }
    for c in &result.calls {
        match serde_json::from_str::<serde_json::Value>(&c.args_json) {
            Ok(serde_json::Value::Object(_)) => {}
            Ok(_) => {
                return Err(format!(
                    "tool call {}: arguments must be a JSON object",
                    c.name
                ));
            }
            Err(e) => return Err(format!("tool call {}: invalid argument JSON: {e}", c.name)),
        }
    }
    let (calls, refused): (Vec<_>, Vec<_>) = result
        .calls
        .iter()
        .cloned()
        .partition(|call| known_tool(&call.name, allow_shell));
    Ok((calls, refused))
}
