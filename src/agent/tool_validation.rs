//! Tool-call validation shared by the normal and forced tool-round paths.

use super::*;

/// A tool call is valid only when its id is unique across the whole attempt
/// (`seen`), the name is a known *and enabled* tool, and the arguments are a
/// JSON object.
pub(super) fn validate_calls(
    seen: &mut std::collections::HashSet<String>,
    result: &LlmResult,
    allow_shell: bool,
) -> Result<Vec<ToolCall>, String> {
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
    for c in &result.calls {
        if !known_tool(&c.name, allow_shell) {
            return Err(format!("unknown or disabled tool {}", c.name));
        }
    }
    Ok(result.calls.clone())
}

/// Whether a tool name is known and (for shell) enabled.
pub fn known_tool(name: &str, allow_shell: bool) -> bool {
    matches!(
        name,
        "read_file" | "write_file" | "replace" | "list_directory" | "search_file_content"
    ) || (name == "run_shell_command" && allow_shell)
}
