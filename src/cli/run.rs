use super::*;

/// Run while publishing optional memory-profile boundaries.
pub fn run_profiled(
    args: Args,
    profiler: Option<crate::memory_profile::Profiler>,
) -> Result<RunOutcome, AppError> {
    // Issue 88: install the cancellation handlers before anything else so a `kill -TERM` on this
    // worker takes the active tool's process group with it. Best-effort; a platform that rejects
    // the registration still runs the turn.
    let _ = crate::process::install_cancellation_signal_handlers();
    let session_id =
        SessionId::parse(&args.session).map_err(|m| AppError::new(Code::Usage, "session", m))?;
    // Validate raw CLI limits before reading a prompt or resolving settings.  In
    // particular, invalid limits must not trigger stdin, profile/config, backend,
    // credential, or keychain access (issue 60).
    validate_cli_limits(&args)?;
    let prompt = match args.prompt.clone() {
        Some(prompt) => prompt,
        None => read_stdin_prompt()?,
    };
    // Resolve every settings layer before production dependencies can read credentials.
    let settings = resolve_settings(&args)?;
    let dependencies = RuntimeDependencies::production()
        .map_err(|error| AppError::new(Code::Config, "config-home", error))?;
    let mut profile = resolve_profile(&args, settings.paths.config_root.value.as_path())?;
    apply_runtime_settings(&mut profile, &settings)?;
    profile_event(&profiler, "profile_parsed", Default::default())?;
    let cwd = resolve_cwd(&args)?;
    let constructed = construct_backend(
        &profile,
        &session_id,
        &dependencies,
        args.profile_load.is_some(),
        args.allow_insecure_http,
    )
    .map_err(|error| AppError::new(Code::Config, "model-config", error))?;
    let agent = build_agent(
        &args,
        &profile,
        &settings,
        constructed,
        &cwd,
        profiler.clone(),
    )?;
    let store = load_session_store_in(&session_id, dependencies.config_home())
        .map_err(|error| AppError::new(Code::Session, "session-store", error))?;
    profile_event(&profiler, "session_store_opened", Default::default())?;
    let _ = store.take_profile_metrics();
    let reserved = store
        .start_request_with_workspace(
            args.turn,
            args.branch.as_deref(),
            &prompt,
            &cwd,
            agent.workspace_cap(),
        )
        .map_err(|error| AppError::new(Code::Turn, "turn", error.to_string()))?;
    let metrics = store.take_profile_metrics();
    profile_event(
        &profiler,
        "reservation_complete",
        crate::memory_profile::EventData {
            branch_count: store.profile_branch_count(),
            round_count: Some(0),
            session_slot_input_bytes: Some(metrics.input_bytes),
            session_slot_output_bytes: Some(metrics.output_bytes),
            ..Default::default()
        },
    )?;
    let output_caps = agent.output_caps();
    let run = agent.run(&store, &reserved).map_err(agent_error)?;
    Ok(RunOutcome {
        session: session_id,
        session_dir: store.session_dir().to_path_buf(),
        run,
        output_caps,
    })
}

fn resolve_cwd(args: &Args) -> Result<PathBuf, AppError> {
    let cwd = match &args.cwd {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|error| {
            AppError::new(Code::Usage, "cwd", format!("cannot resolve cwd: {error}"))
        })?,
    };
    if !cwd.is_dir() {
        return Err(AppError::new(
            Code::Usage,
            "cwd-not-dir",
            format!("cwd is not a directory: {}", cwd.display()),
        ));
    }
    Ok(cwd.canonicalize().unwrap_or(cwd))
}

/// Validate CLI-provided limits without resolving a profile or any other settings layer.
///
/// The settings resolver performs the same validation for every source.  It cannot be
/// the first validation point, though: resolving settings reads profile/config layers,
/// whereas the CLI contract requires malformed command-line limits to fail before even
/// consuming stdin.
fn validate_cli_limits(args: &Args) -> Result<(), AppError> {
    if let Some(value) = args.max_tool_calls {
        crate::settings::validate_max_tool_calls(value)
            .map_err(|message| AppError::new(Code::Usage, "max-tool-calls", message))?;
    }
    if let Some(raw) = args.turn_time.as_deref() {
        crate::settings::parse_turn_time(raw)
            .map_err(|message| AppError::new(Code::Usage, "turn-time", message))?;
    }
    Ok(())
}

fn build_agent(
    args: &Args,
    profile: &Profile,
    settings: &Settings,
    constructed: crate::model_api::registry::ConstructedBackend,
    cwd: &std::path::Path,
    profiler: Option<crate::memory_profile::Profiler>,
) -> Result<CodingAgent, AppError> {
    let max_tool_calls = match settings.budgets.max_tool_calls.value {
        -1 => None,
        value => Some(usize::try_from(value).map_err(|_| {
            AppError::new(
                Code::Config,
                "settings-resolve",
                "resolved max-tool-calls is invalid",
            )
        })?),
    };
    let turn_time = settings.budgets.turn_time.value;
    let mut agent = CodingAgent::new_with_backend(constructed.backend, cwd, args.allow_shell)
        .map_err(|error| AppError::new(error.code, error.key, error.message))?
        .with_secrets(constructed.secret_values)
        .with_context_limit(constructed.context_limit)
        .with_max_rounds(constructed.max_rounds)
        .with_max_tool_calls(max_tool_calls)
        .with_turn_time(turn_time)
        .with_shell_timeouts(
            std::time::Duration::from_secs(
                profile
                    .ephemeral
                    .shell_default_timeout_seconds
                    .unwrap_or(crate::profile::DEFAULT_SHELL_TIMEOUT_SECONDS),
            ),
            std::time::Duration::from_secs(
                profile
                    .ephemeral
                    .shell_max_timeout_seconds
                    .unwrap_or(crate::profile::DEFAULT_SHELL_TIMEOUT_SECONDS),
            ),
        )
        .with_output_caps(resolved_output_caps(settings))
        .with_profiler(profiler);
    agent.prompt_notes = CodingAgent::prompt_reason_note(profile);
    Ok(agent)
}

/// Resolve the output caps (issue 77) the agent enforces and the run reports: per-result
/// shell/tool caps and the aggregate per-turn tool-output cap. The resolver has already
/// validated that neither per-result cap exceeds the turn cap.
fn resolved_output_caps(settings: &Settings) -> crate::agent::OutputCaps {
    let cap = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
    crate::agent::OutputCaps {
        shell: cap(settings.budgets.max_shell_output.value),
        tool: cap(settings.budgets.max_tool_output.value),
        turn: cap(settings.budgets.max_turn_output.value),
    }
}

fn agent_error(error: crate::agent::AgentError) -> AppError {
    if error.code == Code::Profiling {
        return AppError::profiling_at(error.key, error.message);
    }
    let mut app =
        AppError::new(error.code, error.key, error.message).with_envelope_code(error.envelope_code);
    if error.terminal_outcome.is_some() {
        // The run declared its own terminal verdict (issues 146 and 153); carry it into
        // the stdout envelope so a headless caller can branch on this condition alone.
        app.terminal_outcome = error.terminal_outcome;
    } else if error.key == crate::agent::MALFORMED_TOOL_CALL_KEY {
        // The malformed-tool-call collapse (issue 146) declares the same verdict through
        // its typed key, so a caller can retry on this condition alone.
        app.terminal_outcome = Some(crate::agent::MALFORMED_TOOL_CALL_KEY);
    }
    app
}
