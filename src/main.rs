//! Binary entry: explicit profile finalization precedes the single stdout envelope.

use llxprt_code_rs::{cli, memory_profile::Profiler};

fn main() {
    if std::env::args_os().skip(1).eq(["--session-state-versions"]) {
        println!("{}", cli::session_state_versions());
        return;
    }
    let session_hint = cli::session_hint();
    let args = cli::parse_args_fallback(&session_hint);
    if let Err(message) = llxprt_code_rs::session::SessionId::parse(&args.session) {
        let outcome = Err(cli::AppError::new(cli::Code::Usage, "session", message));
        println!("{}", cli::json(&outcome, &session_hint));
        std::process::exit(cli::Code::Usage as i32);
    }
    if let Err(error) = cli::validate_cli_limits(&args) {
        let outcome = Err(error);
        println!("{}", cli::json(&outcome, &session_hint));
        std::process::exit(cli::exit_code(&outcome));
    }
    if render_transcript(&args) {
        return;
    }
    if args.oauth_login {
        match cli::local_oauth_login() {
            Ok(()) => println!(
                "{}",
                serde_json::json!({"session_id": args.session, "status": "ok", "oauth_storage": "local"})
            ),
            Err(error) => {
                let outcome = Err(error);
                println!("{}", cli::json(&outcome, &session_hint));
                std::process::exit(cli::exit_code(&outcome));
            }
        }
        return;
    }
    if args.print_config {
        match cli::print_config(&args) {
            Ok(settings) => println!("{settings}"),
            Err(error) => {
                println!("{}", cli::json(&Err(error), &session_hint));
                std::process::exit(cli::Code::Config as i32);
            }
        }
        return;
    }
    let profiler = match args.mem_profile.as_deref() {
        Some(path) => match Profiler::initialize(path) {
            Ok(profiler) => Some(profiler),
            Err(error) => exit_profile_error(&session_hint, error, "ok"),
        },
        None => None,
    };
    let mut outcome = cli::run_profiled(args, profiler.clone());
    let mut summary = None;
    if let Some(profiler) = profiler {
        if outcome
            .as_ref()
            .err()
            .is_none_or(|error| error.code != cli::Code::Profiling)
        {
            let pre_exit = profiler.event("pre_exit", Default::default());
            let result = pre_exit.and_then(|()| profiler.finalize(outcome_class(&outcome)));
            match result {
                Ok(done) => summary = Some(done),
                Err(error) => {
                    let status = session_status(&outcome);
                    outcome = Err(profile_error(error, status, &outcome));
                }
            }
        }
    }
    let value = cli::json(&outcome, &session_hint);
    let code = cli::exit_code(&outcome);
    println!("{value}");
    // Default thinking reserves stderr for transcript JSONL; profiling stays in its file.
    let _ = summary;
    std::process::exit(code);
}

fn render_transcript(args: &cli::Args) -> bool {
    if let Some(llxprt_code_rs::transcript::Command::Transcript(options)) = &args.command {
        match options.render() {
            Ok(output) => print!("{output}"),
            Err(error) => {
                let code = error.code as i32;
                let error = cli::AppError::new(error.code, error.key, error.message);
                println!("{}", cli::json(&Err(error), &options.session));
                std::process::exit(code);
            }
        }
        true
    } else {
        false
    }
}

fn outcome_class(outcome: &Result<cli::RunOutcome, cli::AppError>) -> &'static str {
    match outcome {
        Ok(_) => "ok",
        Err(error) => match error.code {
            cli::Code::Config => "config",
            cli::Code::Session => "session",
            cli::Code::Model => "model",
            cli::Code::Turn | cli::Code::Usage => "turn",
            cli::Code::Profiling => "turn",
        },
    }
}

fn session_status(outcome: &Result<cli::RunOutcome, cli::AppError>) -> String {
    match outcome {
        Ok(_) => "ok".into(),
        Err(error) => error.key.to_string(),
    }
}

fn profile_error(
    error: llxprt_code_rs::memory_profile::ProfilingError,
    status: String,
    outcome: &Result<cli::RunOutcome, cli::AppError>,
) -> cli::AppError {
    let mut app = cli::AppError::profiling(error);
    app.session_status = Some(status.into_boxed_str());
    app.observations = Some(Box::new(cli::FailureObservations {
        telemetry: None,
        retry_attempts: match outcome {
            Ok(run) => Some(run.run.retry_attempts),
            Err(error) => error.retry_attempts(),
        },
    }));
    app
}

fn exit_profile_error(
    session: &str,
    error: llxprt_code_rs::memory_profile::ProfilingError,
    status: &str,
) -> ! {
    let mut app = cli::AppError::profiling(error);
    app.session_status = Some(status.into());
    print!(
        "{}",
        String::from_utf8_lossy(&cli::envelope(&Err(app), session).to_line())
    );
    std::process::exit(cli::Code::Profiling as i32)
}
