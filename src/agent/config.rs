use super::{CodingAgent, OutputCaps};

/// Build the coding-agent system prompt.
pub fn coding_system_prompt(
    cwd: &std::path::Path,
    reasoning: &str,
    shell_on: bool,
    tool_budget: Option<usize>,
) -> String {
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
    let budget_note = match tool_budget {
        Some(n) => format!(
            "\nTool budget: at most {n} tool calls for this task. Every tool round reports what remains; when few remain, stop exploring and produce your final summary."
        ),
        None => "\nTool budget: no fixed tool-call limit this task (round and size caps still apply).".to_string(),
    };
    format!(
        "You are a coding agent working in {}.\n{shell_note}{reasoning_note}{budget_note}",
        cwd.display()
    )
}

/// The per-turn round-limit diagnostic. The loop enforces its **effective** round cap
/// (`max_rounds`); a profile override lowers that cap, so the diagnostic names the
/// effective cap, never a hardcoded constant.
pub fn round_limit_message(max_rounds: usize) -> String {
    format!("turn would exceed the {max_rounds} round cap; give a final summary instead")
}

impl super::CodingAgent {
    /// Override the agent's conservative per-request context budget (tests drive budget
    /// enforcement with an explicit token budget instead of a profile).
    pub fn with_context_limit(mut self, context_limit: Option<u64>) -> CodingAgent {
        self.context_limit = context_limit;
        self
    }

    /// Override the per-turn round cap (tests drive round-cap enforcement with explicit
    /// budgets instead of the uncapped default).
    pub fn with_max_rounds(mut self, max_rounds: usize) -> CodingAgent {
        self.max_rounds = max_rounds;
        self
    }

    /// Override the resolved per-prompt tool-call budget (`None` = unlimited).
    pub fn with_max_tool_calls(mut self, max_tool_calls: Option<usize>) -> CodingAgent {
        self.max_tool_calls = max_tool_calls;
        self
    }

    /// Override the wall-clock turn budget (`None` = no time limit).
    pub fn with_turn_time(mut self, budget: Option<std::time::Duration>) -> CodingAgent {
        self.turn_time_budget = budget;
        self
    }

    /// Set validated profile shell timeouts. Both remain finite and the parser
    /// guarantees the default does not exceed the per-command ceiling.
    pub fn with_shell_timeouts(
        mut self,
        default_timeout: std::time::Duration,
        max_timeout: std::time::Duration,
    ) -> CodingAgent {
        self.shell_default_timeout = default_timeout;
        self.shell_max_timeout = max_timeout;
        self
    }

    /// Override the resolved output caps (issue 77): per-result shell/tool caps and the
    /// aggregate per-turn tool-output bound enforced by the turn loop.
    pub fn with_output_caps(mut self, caps: OutputCaps) -> CodingAgent {
        self.output_caps = caps;
        self
    }

    /// The resolved output caps this agent enforces.
    pub fn output_caps(&self) -> OutputCaps {
        self.output_caps
    }

    /// Build the tool configuration from the resolved output and shell-timeout policy.
    pub(super) fn tools_config(&self, shell_on: bool) -> Result<crate::tools::ToolConfig, String> {
        Ok(crate::tools::ToolConfig {
            ws: self.workspace.try_clone()?,
            max_output_bytes: self.output_caps.tool,
            shell: crate::tools::ShellConfig {
                max_shell_output: self.output_caps.shell,
                default_shell_timeout: self.shell_default_timeout,
                max_shell_timeout: self.shell_max_timeout,
                allow_shell: shell_on,
            },
        })
    }

    /// Reason-effort (or other request-side) profile notes to append to the system
    /// prompt. This is a text note about the author's intent; the transport never
    /// forwards a reasoning field. The note is bounded (a profile value is capped at
    /// [`crate::redact::MAX_PROMPT_NOTE_BYTES`] and the accumulated prompt text
    /// carries its own documented cap in [`crate::redact::PROMPT_NOTE_CAP_MESSAGE`]).
    pub fn prompt_reason_note(profile: &crate::profile::Profile) -> Option<String> {
        let s = profile
            .ephemeral
            .prompt_notes
            .get("reasoning:reasoning.effort")?;
        let note = format!("reasoning effort requested by profile (prompt note only): {s}");
        if note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
            return Some(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
        }
        Some(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_limit_message_names_the_effective_cap() {
        assert_eq!(
            round_limit_message(7),
            "turn would exceed the 7 round cap; give a final summary instead"
        );
    }
}
