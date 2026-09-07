//! Bounded shell-tool execution and diagnostics.

use super::*;

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
    let requested = arg_u64(args, "timeout_seconds")?
        .unwrap_or(default_timeout.as_secs())
        .max(1);
    let (effective, clamped) = (
        requested.min(max_timeout.as_secs()),
        requested > max_timeout.as_secs(),
    );
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
    // Inline clamp note for the timeout diagnostic only; the success path appends
    // the bracketed note below. Built as a plain binding so `format!` input stays
    // free of control flow (xtask counts macro control flow as unmeasured code).
    let clamp_note = if clamped {
        format!(" (requested timeout {requested}s; effective timeout {effective}s)")
    } else {
        String::new()
    };
    let s = if o.timed_out {
        format!(
            "command timed out after {} ms{}; output:\n{}",
            timeout.as_millis(),
            clamp_note,
            combined.trim_end()
        )
    } else {
        match o.status {
            Some(0) => combined.trim_end().to_string(),
            Some(code) => format!(
                "command exited with {code}; output:\n{}",
                combined.trim_end()
            ),
            None => format!(
                "command was killed by a signal; output:\n{}",
                combined.trim_end()
            ),
        }
    };
    let s = if clamped && !o.timed_out {
        format!("{s}\n[requested timeout {requested}s; effective timeout {effective}s]")
    } else {
        s
    };
    let bounded = truncate(&s, max_output);
    if o.timed_out {
        Err(bounded)
    } else {
        match o.status {
            Some(0) => Ok(bounded),
            Some(_) | None => Err(bounded),
        }
    }
}
