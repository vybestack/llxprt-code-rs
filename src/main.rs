//! Binary entry: explicit profile finalization precedes the single stdout envelope.

use llxprt_code_rs::{cli, envelope::Envelope, memory_profile::Profiler};

fn main() {
    let session_hint = cli::session_hint();
    let args = cli::parse_args_fallback();
    if let Err(message) = llxprt_code_rs::session::SessionId::parse(&args.session) {
        let outcome = Err(cli::AppError::new(cli::Code::Usage, "session", message));
        println!("{}", cli::json(&outcome, &session_hint));
        std::process::exit(cli::Code::Usage as i32);
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
                    outcome = Err(profile_error(error, status));
                }
            }
        }
    }
    let value = cli::json(&outcome, &session_hint);
    let code = cli::exit_code(&outcome);
    println!("{value}");
    if let Some(summary) = summary {
        eprintln!("{}", summary.stderr_line());
    }
    std::process::exit(code);
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
) -> cli::AppError {
    let mut app = cli::AppError::profiling(error);
    app.session_status = Some(status);
    app
}

fn exit_profile_error(
    session: &str,
    error: llxprt_code_rs::memory_profile::ProfilingError,
    status: &str,
) -> ! {
    let envelope = Envelope::profiling_error(session, error.message, error.stage, status);
    print!("{}", String::from_utf8_lossy(&envelope.to_line()));
    std::process::exit(cli::Code::Profiling as i32)
}
