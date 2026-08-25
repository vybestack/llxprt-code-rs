use crate::session::LEASE_SECONDS;

/// Safety margin subtracted from the session lease when validating a request timeout.
pub const TIMEOUT_LEASE_MARGIN_SECONDS: u64 = 60;

/// Validate a request timeout against the session lease, preferring a request that always fits
/// inside one lease: without an independent heartbeat a request of `lease` seconds could outlive
/// its own lease, so a value at or above the lease minus [`TIMEOUT_LEASE_MARGIN_SECONDS`]
/// is refused up front.
pub fn validate_timeout(timeout: Option<std::time::Duration>) -> Result<(), String> {
    let lease = LEASE_SECONDS;
    let max = lease.saturating_sub(TIMEOUT_LEASE_MARGIN_SECONDS);
    if let Some(timeout) = timeout {
        if timeout.as_secs() >= max {
            return Err(format!(
                "request timeout {}s must be below the session lease ({lease}s minus a {TIMEOUT_LEASE_MARGIN_SECONDS}s margin)",
                timeout.as_secs()
            ));
        }
    }
    Ok(())
}

/// FNV-1a hash of a prompt, used as a compact identity for replay detection.
pub fn prompt_digest(prompt: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in prompt.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Build the coding-agent system prompt.
pub fn coding_system_prompt(cwd: &std::path::Path, reasoning: &str, shell_on: bool) -> String {
    let shell_note = if shell_on {
        "You may run shells and install nothing outside the workspace. Shell commands run with the general user's privileges and can execute arbitrary code, so keep every command confined to this project and never exfiltrate data."
    } else {
        "Shell execution is disabled; edit files directly and do not invoke the shell tool."
    };
    let reasoning_note = if reasoning.is_empty() {
        String::new()
    } else {
        format!("\nReasoning context: {reasoning}\n")
    };
    format!(
        "You are a coding agent working in {}.\n{shell_note}{reasoning_note}",
        cwd.display()
    )
}
