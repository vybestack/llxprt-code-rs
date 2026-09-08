//! Export the current host-level #220 tool guidance, not an outbound provider request.
use llxprt_code_rs::agent::coding_system_prompt;
use llxprt_code_rs::tools::tool_specs;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let cwd = args.next().ok_or("expected workspace path")?;
    if args.next().is_some() {
        return Err("expected exactly one workspace path".into());
    }
    let tools: Vec<_> = tool_specs(true)
        .into_iter()
        .map(|tool| {
            json!({"name": tool.name, "description": tool.description,
                   "properties": tool.properties})
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "qualification": "host prompt and ToolSpec values, not serialized wire capture",
            "system": coding_system_prompt(std::path::Path::new(&cwd), "", true, None),
            "tools": tools,
        }))?
    );
    Ok(())
}
