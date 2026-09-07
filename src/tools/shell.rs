//! Bounded shell-tool execution and diagnostics.

use super::*;

fn shell_timeout_request(
    args: &BTreeMap<String, JsonValue>,
    default_timeout: std::time::Duration,
    max_timeout: std::time::Duration,
) -> Result<(u64, String, bool), String> {
    match args.get("timeout_seconds") {
        None => {
            let seconds = default_timeout.as_secs().max(1);
            Ok((seconds, format!("{seconds}s"), false))
        }
        Some(JsonValue::Number(value)) if value.as_i64() == Some(-1) => {
            Ok((max_timeout.as_secs(), "unlimited".to_string(), true))
        }
        Some(JsonValue::Number(value)) => {
            let seconds = value
                .as_u64()
                .filter(|seconds| *seconds > 0)
                .ok_or_else(|| {
                    "argument 'timeout_seconds' must be -1 (unlimited) or a positive integer"
                        .to_string()
                })?;
            Ok((seconds, format!("{seconds}s"), false))
        }
        Some(_) => Err("argument 'timeout_seconds' must be an integer".into()),
    }
}

/// Run a shell command via the shared bounded runner. Nonzero exit, a signal, or a timeout
/// is an `Err` carrying the captured output (the model sees `ok=false`).
pub(super) fn shell_tool(
    fd: i32,
    args: &BTreeMap<String, JsonValue>,
    default_timeout: std::time::Duration,
    max_timeout: std::time::Duration,
    max_output: usize,
) -> Result<String, String> {
    reject_unknown(args, &["command", "timeout_seconds"])?;
    let command = arg_str(args, "command", true)?.unwrap();
    if command.trim().is_empty() {
        return Err("command must not be empty".into());
    }
    let (requested, requested_label, unlimited) =
        shell_timeout_request(args, default_timeout, max_timeout)?;
    let effective = requested.min(max_timeout.as_secs());
    let clamped = unlimited || requested > max_timeout.as_secs();
    let timeout = std::time::Duration::from_secs(effective);
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
        format!(" (requested timeout {requested_label}; effective timeout {effective}s)")
    } else {
        String::new()
    };
    let framing = if o.timed_out {
        format!(
            "command timed out after {} ms{}; output:\n",
            timeout.as_millis(),
            clamp_note,
        )
    } else {
        match o.status {
            Some(0) if clamped => {
                format!("[requested timeout {requested_label}; effective timeout {effective}s]\n")
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
