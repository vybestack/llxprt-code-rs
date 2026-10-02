//! Bounded shell-tool execution and diagnostics.

use super::*;

fn shell_timeout_request(
    args: &BTreeMap<String, JsonValue>,
    policy: ShellTimeoutPolicy,
) -> Result<(Option<std::time::Duration>, String, bool), String> {
    let requested = match args.get("timeout_seconds") {
        None => policy.default,
        Some(JsonValue::Number(value)) if value.as_i64() == Some(-1) => None,
        Some(JsonValue::Number(value)) => Some(std::time::Duration::from_secs(
            value
                .as_u64()
                .filter(|seconds| *seconds > 0)
                .ok_or_else(|| {
                    "argument 'timeout_seconds' must be -1 (unlimited) or a positive integer"
                        .to_string()
                })?,
        )),
        Some(_) => return Err("argument 'timeout_seconds' must be an integer".into()),
    };
    let effective = if args.contains_key("timeout_seconds") {
        policy.clamp(requested)
    } else {
        policy.resolve(None)
    };
    let label = requested.map_or_else(
        || "unlimited".to_string(),
        |timeout| format!("{}s", timeout.as_secs()),
    );
    Ok((effective, label, effective != requested))
}

/// Run a shell command via the shared bounded runner. Nonzero exit, a signal, or a timeout
/// is an `Err` carrying the captured output (the model sees `ok=false`).
pub(super) fn shell_tool(
    fd: i32,
    args: &BTreeMap<String, JsonValue>,
    policy: ShellTimeoutPolicy,
    max_output: usize,
) -> Result<String, String> {
    reject_unknown(args, &["command", "timeout_seconds"])?;
    let command = arg_str(args, "command", true)?.unwrap();
    if command.trim().is_empty() {
        return Err("command must not be empty".into());
    }
    let (timeout, requested_label, clamped) = shell_timeout_request(args, policy)?;
    let o = crate::process::run_cmd(crate::process::CmdSpec {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        cwd: None,
        cwd_fd: Some(fd),
        env_add: Vec::new(),
        timeout,
        max_output,
    })?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // Every model-visible shell string (success or failure diagnostic) is bounded as one
    // value, framing and combined output included, to `max_output`.
    let clamp_note = if clamped {
        format!(
            " (requested timeout {requested_label}; effective timeout {}s)",
            timeout.map_or(0, |timeout| timeout.as_secs())
        )
    } else {
        String::new()
    };
    let framing = if o.timed_out {
        format!(
            "command timed out after {} ms{}; output:\n",
            timeout.map_or(0, |timeout| timeout.as_millis()),
            clamp_note,
        )
    } else {
        match o.status {
            Some(0) if clamped => {
                format!(
                    "[requested timeout {requested_label}; effective timeout {}s]\n",
                    timeout.map_or(0, |timeout| timeout.as_secs())
                )
            }
            Some(0) => String::new(),
            Some(code) => format!("command exited with {code}{clamp_note}; output:\n"),
            None => format!("command was killed by a signal{clamp_note}; output:\n"),
        }
    };
    let bounded = if clamped {
        // Reserve the diagnostic before truncating payload; large output must not
        // displace the requested/effective timeout disclosure. Tiny budgets still
        // bound the framing itself, just like every other tool diagnostic.
        let framing = truncate(&framing, max_output);
        let payload = truncate(combined.trim_end(), max_output - framing.len());
        format!("{framing}{payload}")
    } else {
        truncate(&format!("{framing}{}", combined.trim_end()), max_output)
    };
    if o.timed_out {
        Err(bounded)
    } else {
        match o.status {
            Some(0) => Ok(bounded),
            Some(_) | None => Err(bounded),
        }
    }
}
