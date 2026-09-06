use super::*;

/// Run while publishing optional memory-profile boundaries.
pub fn run_profiled(
    args: Args,
    profiler: Option<crate::memory_profile::Profiler>,
) -> Result<RunOutcome, AppError> {
    let session_id =
        SessionId::parse(&args.session).map_err(|m| AppError::new(Code::Usage, "session", m))?;
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
    let run = agent.run(&store, &reserved).map_err(agent_error)?;
    Ok(RunOutcome {
        session: session_id,
        session_dir: store.session_dir().to_path_buf(),
        run,
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
        .with_profiler(profiler);
    agent.prompt_notes = CodingAgent::prompt_reason_note(profile);
    Ok(agent)
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
