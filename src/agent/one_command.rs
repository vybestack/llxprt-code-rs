//! Dispatch contract for explicit, backtick-delimited one-command missions.
//! This is not shell authorization: existing permissions, budgets and execution
//! policies still apply. Ordinary prompts retain the ordinary agent loop.
use super::*;

const REFUSED: &str = "one-command-refused";
const FAILED: &str = "one-command-failed";

/// The deliberately narrow syntax avoids guessing commands from arbitrary prose
/// or from quoted history. Match only an imperative at the start of this turn.
pub(super) fn command(prompt: &str) -> Result<Option<&str>, &'static str> {
    const PREFIX: &str = "run exactly one shell command:";
    let prompt = prompt.trim_start();
    if !prompt
        .get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
    {
        return Ok(None);
    }
    let rest = prompt[PREFIX.len()..].trim_start();
    let rest = rest
        .strip_prefix('`')
        .ok_or("expected a backtick-delimited command")?;
    let (command, _) = rest.split_once('`').ok_or("unterminated command")?;
    if command.trim().is_empty() || command.contains('\n') || command.contains('\r') {
        return Err("expected a nonempty single-line command");
    }
    Ok(Some(command))
}

impl Turn<'_> {
    pub(super) fn mission_command<'a>(
        &self,
        store: &SessionStore,
        reserved: &'a ReservedRequest,
    ) -> Result<Option<&'a str>, AgentError> {
        let command = command(&reserved.prompt)
            .map_err(|message| self.mission_error(store, reserved, REFUSED, message, &[]))?;
        if command.is_some() && (!self.allow_shell || self.max_tool_calls == Some(0)) {
            return Err(self.mission_error(
                store,
                reserved,
                REFUSED,
                "one-command mission requires shell permission and a nonzero tool-call budget",
                &[],
            ));
        }
        Ok(command)
    }

    pub(super) fn run_one_command(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        command: &str,
        config: &crate::tools::ToolConfig,
        mut attempt: AttemptState,
    ) -> Result<CompletedRun, AgentError> {
        let reply = attempt.current.as_ref().expect("first completion");
        let calls = validate_calls(&mut std::collections::HashSet::new(), reply)
            .map_err(|message| self.mission_error(store, reserved, REFUSED, &message, &[]))?;
        if calls.len() != 1 || calls[0].name != "run_shell_command" {
            return Err(self.mission_error(
                store,
                reserved,
                REFUSED,
                "one-command mission refused: expected only the named shell command; no tools ran",
                &[],
            ));
        }
        let args = parse_object_args(&calls[0])
            .map_err(|message| self.mission_error(store, reserved, REFUSED, &message, &[]))?;
        if args.get("command").and_then(JsonValue::as_str) != Some(command) {
            return Err(self.mission_error(
                store,
                reserved,
                REFUSED,
                "one-command mission refused: command did not match exactly; no tools ran",
                &[],
            ));
        }
        // Validate the entire batch before the ordinary executor splits budgets
        // or dispatches any side effect. A mixed batch never runs even its prefix.
        self.execute_tool_round(store, reserved, config, &mut attempt)?;
        let record = &attempt.rounds[0].calls[0];
        if !record.ok {
            return Err(self.mission_error(
                store,
                reserved,
                FAILED,
                &record.result_live,
                &attempt.rounds,
            ));
        }
        let summary = if record.result_live.trim().is_empty() {
            "Named shell command completed successfully (no output).".to_string()
        } else {
            record.result_live.clone()
        };
        attempt.usage.assistant_bytes = attempt.usage.assistant_bytes.saturating_add(summary.len());
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        // The tool result is the summary of record; no model follow-up can ask
        // to inspect an ignored script, repeat the command, or replace its output.
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        self.complete_attempt(store, reserved, summary, attempt)
    }

    fn mission_error(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        key: &'static str,
        message: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        let mut error = self.dead_coded(
            store,
            reserved,
            crate::envelope::Code::Turn,
            key,
            message,
            rounds,
        );
        if error.key == key {
            error.terminal_outcome = Some(key);
        }
        error
    }
}

#[cfg(test)]
mod tests {
    use super::command;

    #[test]
    fn recognizes_only_current_turn_imperative_and_preserves_command_bytes() {
        assert_eq!(
            command(" RUN EXACTLY ONE SHELL COMMAND: ` printf hi `."),
            Ok(Some(" printf hi "))
        );
        for prompt in [
            "Read this example: Run exactly one shell command: `echo hi`",
            "Run a shell command: `echo hi`",
            "Implement feature X",
            "`Run exactly one shell command: echo hi`",
        ] {
            assert_eq!(command(prompt), Ok(None));
        }
    }

    #[test]
    fn malformed_explicit_missions_fail_closed() {
        for prompt in [
            "Run exactly one shell command: echo hi",
            "Run exactly one shell command: `",
            "Run exactly one shell command: ``",
            "Run exactly one shell command: `echo\nhi`",
        ] {
            assert!(command(prompt).is_err(), "{prompt}");
        }
    }

    #[test]
    fn zero_agent_budget_refuses_without_backend_or_tools() {
        let _config = crate::agent::tests::shared_config_home();
        let cwd = tempfile::tempdir().unwrap();
        let id = crate::session::SessionId::fresh();
        let store = crate::session::SessionStore::load(&id).unwrap();
        let reserved = store
            .start_request(
                None,
                None,
                "Run exactly one shell command: `touch forbidden`",
                cwd.path(),
            )
            .unwrap();
        let agent = super::CodingAgent::with_backend(
            Box::new(crate::agent::tests::MockBackend::new(vec![])),
            cwd.path().to_path_buf(),
            true,
        )
        .with_max_tool_calls(Some(0));
        let error = agent.run(&store, &reserved).unwrap_err();
        assert_eq!(error.key, "one-command-refused");
        assert_eq!(error.code, crate::envelope::Code::Turn);
        assert_eq!(agent.model_calls(), 0);
        assert!(!cwd.path().join("forbidden").exists());
        assert_eq!(
            store.snapshot().unwrap().branches[0].lifecycle,
            crate::session::Lifecycle::Failed
        );
    }
}
