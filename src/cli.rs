//! Command-line entry point and JSON stdout contract.
//!
//! Every runtime outcome — including failures — emits exactly one JSON object on stdout
//! with a `session_id`. `--help`/`--version` are protocol exceptions (Clap renders
//! them and exits 0). All other outcomes (including Clap usage errors) are exactly one
//! JSON object.

use crate::agent::CodingAgent;
use crate::model::{ProfileResolver, ResolveOutcome};
use crate::model_api::dependencies::RuntimeDependencies;
use crate::model_api::registry::construct_backend;
use crate::profile::Profile;
use crate::session::load_session_store_in;
use crate::session::SessionId;
use clap::Parser;
use std::io::IsTerminal;
use std::path::PathBuf;

/// The maximum number of bytes read from stdin before the prompt is rejected. The cap is
/// applied **while reading**, not after allocation.
const MAX_STDIN_BYTES: usize = crate::session::MAX_PROMPT_BYTES;

/// Exit codes exposed to the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Usage = 2,
    Config = 3,
    Session = 4,
    Model = 5,
    Turn = 6,
}

/// CLI arguments. Doc comments surface in `--help`.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "llxprt-code-rs",
    version,
    about = "Headless coding agent. One JSON object on stdout per run."
)]
pub struct Args {
    /// Session id: a safe identifier of [A-Za-z0-9_-]; no '/', '.', '..'.
    #[arg(long, default_value = "default")]
    pub session: String,

    /// 1-based turn. Omitted appends the next turn after the newest completed branch.
    #[arg(long)]
    pub turn: Option<u32>,

    /// Branch id to continue from (deterministic continuation). Must already exist.
    #[arg(long)]
    pub branch: Option<String>,

    /// Named llxprt profile (from the llxprt-code profiles dir).
    #[arg(long, conflicts_with = "profile_load")]
    pub profile: Option<String>,

    /// Path to a profile JSON file. Must carry its own auth-key/auth-keyfile.
    #[arg(long, conflicts_with = "profile")]
    pub profile_load: Option<PathBuf>,

    /// Working directory that bounds all tool paths and shell execution.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Prompt. If omitted, read from stdin (entire input).
    #[arg(short, long)]
    pub prompt: Option<String>,

    /// Explicit opt-in to allow plaintext HTTP to a remote host (dsflash-mi300x style).
    #[arg(long)]
    pub allow_insecure_http: bool,

    /// Explicit opt-in to register the run_shell_command tool.
    #[arg(long)]
    pub allow_shell: bool,

    /// Per-prompt tool-call budget: `1..=512`, or `-1` for unlimited. Overrides the
    /// profile's `maxToolCallsPerPrompt`; when omitted, the profile field (then 16) applies.
    #[arg(long, value_name = "N")]
    pub max_tool_calls: Option<i64>,

    /// Wall-clock budget per prompt like `90s`, `30m`, `2h`; `0` disables.
    /// Omitted means no time limit.
    #[arg(long, value_name = "DURATION")]
    pub turn_time: Option<String>,
}

/// Parse a `--turn-time` value: digits plus an `s`/`m`/`h` unit, or a bare
/// `0` (any zero form disables the budget). Bare nonzero numbers, unknown
/// units, and overflowing values are usage errors.
pub(crate) fn parse_turn_time(raw: &str) -> Result<Option<std::time::Duration>, String> {
    let raw = raw.trim();
    let (digits, unit) = match raw.char_indices().rfind(|(_, c)| !c.is_ascii_digit()) {
        Some((idx, c)) => (&raw[..idx], c),
        None => (raw, '\0'),
    };
    let seconds_per_unit = match unit {
        '\0' => {
            if digits == "0" {
                return Ok(None);
            }
            return Err(format!(
                "--turn-time needs an s/m/h unit (got {raw:?}); pass 0 to disable"
            ));
        }
        's' => 1u64,
        'm' => 60,
        'h' => 3600,
        _ => return Err(format!("--turn-time unit must be s, m, or h (got {raw:?})")),
    };
    let Ok(count) = digits.parse::<u64>() else {
        return Err(format!("--turn-time needs an integer count (got {raw:?})"));
    };
    let seconds = count
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| format!("--turn-time {raw:?} overflows"))?;
    if seconds == 0 {
        return Ok(None);
    }
    Ok(Some(std::time::Duration::from_secs(seconds)))
}

/// Outcome of a successful invocation.
pub struct RunOutcome {
    pub session: SessionId,
    pub session_dir: std::path::PathBuf,
    pub run: crate::agent::CompletedRun,
}

/// Run the full workflow from parsed args.
pub fn run(args: Args) -> Result<RunOutcome, AppError> {
    let session_id =
        SessionId::parse(&args.session).map_err(|m| AppError::new(Code::Usage, "session", m))?;

    let prompt = match args.prompt.clone() {
        Some(p) => p,
        None => read_stdin_prompt()?,
    };

    let dependencies = RuntimeDependencies::production()
        .map_err(|error| AppError::new(Code::Config, "config-home", error))?;
    let profile = resolve_profile(&args, dependencies.config_home().as_path())?;

    let cwd = match &args.cwd {
        Some(p) => p.clone(),
        None => std::env::current_dir()
            .map_err(|e| AppError::new(Code::Usage, "cwd", format!("cannot resolve cwd: {e}")))?,
    };
    if !cwd.is_dir() {
        return Err(AppError::new(
            Code::Usage,
            "cwd-not-dir",
            format!("cwd is not a directory: {}", cwd.display()),
        ));
    }
    // Canonicalize now so the pinned path and every tool-resolved path agree.
    let cwd = cwd.canonicalize().unwrap_or(cwd.clone());

    let constructed = construct_backend(
        &profile,
        &session_id,
        &dependencies,
        args.profile_load.is_some(),
        args.allow_insecure_http,
    )
    .map_err(|error| AppError::new(Code::Config, "model-config", error))?;

    // Construct the selected backend before session reservation. Credential failures and
    // provider-specific configuration errors therefore cannot create session artifacts.
    let reason_note = CodingAgent::prompt_reason_note(&profile);
    let cli_max_tool_calls = match args.max_tool_calls {
        None | Some(-1) | Some(1..=512) => args.max_tool_calls,
        Some(n) => {
            return Err(AppError::new(
                Code::Usage,
                "max-tool-calls",
                format!("--max-tool-calls must be -1 or an integer from 1 through 512 (got {n})"),
            ));
        }
    };
    let max_tool_calls = crate::profile::resolve_max_tool_calls(
        cli_max_tool_calls,
        profile.ephemeral.max_tool_calls_per_prompt,
    );
    let turn_time = match &args.turn_time {
        None => None,
        Some(raw) => parse_turn_time(raw)
            .map_err(|message| AppError::new(Code::Usage, "turn-time", message))?,
    };
    let mut agent = CodingAgent::new_with_backend(constructed.backend, &cwd, args.allow_shell)
        .map_err(|e| AppError::new(e.code, e.key, e.message))?
        .with_secrets(constructed.secret_values)
        .with_context_limit(constructed.context_limit)
        .with_max_rounds(constructed.max_rounds)
        .with_max_tool_calls(max_tool_calls)
        .with_turn_time(turn_time);
    agent.prompt_notes = reason_note;
    let store = load_session_store_in(&session_id, dependencies.config_home())
        .map_err(|e| AppError::new(Code::Session, "session-store", e))?;

    let reserved = store
        .start_request_with_workspace(
            args.turn,
            args.branch.as_deref(),
            &prompt,
            &cwd,
            agent.workspace_cap(),
        )
        .map_err(|e| AppError::new(Code::Turn, "turn", e.to_string()))?;

    let run = agent
        .run(&store, &reserved)
        .map_err(|e| AppError::new(e.code, e.key, e.message))?;

    Ok(RunOutcome {
        session: session_id,
        session_dir: store.session_dir().to_path_buf(),
        run,
    })
}

/// Serialize an outcome to its exactly-one-JSON value.
pub fn to_json(outcome: &Result<RunOutcome, AppError>) -> serde_json::Value {
    match outcome {
        Ok(o) => serde_json::json!({
            "session_id": o.session.id,
            "session_dir": o.session_dir.display().to_string(),
            "turn": o.run.turn,
            "attempt": o.run.attempt,
            "branch_id": o.run.branch_id,
            "branch": o.run.branch,
            "replayed": o.run.replayed,
            "status": o.run.status,
            "summary": o.run.summary,
            "tool_calls": o.run.tool_count,
            "prompt_digest": o.run.prompt_digest,
        }),
        Err(e) => serde_json::json!({
            "session_id": "default",
            "status": "error",
            "error": { "code": e.key, "message": e.message },
        }),
    }
}

/// Fill the validated session id into an error payload so even failures identify their
/// session without trusting argv order.
pub fn with_session(value: serde_json::Value, id: &str) -> serde_json::Value {
    let mut value = value;
    if value.get("session_id").and_then(|s| s.as_str()) == Some("default") {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "session_id".to_string(),
                serde_json::Value::String(id.to_string()),
            );
        }
    }
    value
}

/// Exit code for an outcome.
pub fn exit_code(outcome: &Result<RunOutcome, AppError>) -> i32 {
    match outcome {
        Ok(_) => 0,
        Err(e) => e.code as i32,
    }
}

/// Single JSON for an outcome; error payloads are filled with the session hint by the
/// caller.
pub fn json(outcome: &Result<RunOutcome, AppError>, session_hint: &str) -> serde_json::Value {
    match outcome {
        Ok(_) => to_json(outcome),
        Err(_) => with_session(to_json(outcome), session_hint),
    }
}

/// Best-effort session id for error payloads: the validated `--session` value in argv.
pub fn session_hint() -> String {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].to_string_lossy().into_owned();
        if let Some(v) = raw.strip_prefix("--session=") {
            if crate::session::is_safe_component(v) {
                return v.to_string();
            }
        }
        if raw == "--session" {
            if let Some(v) = args.get(i + 1) {
                let v = v.to_string_lossy().into_owned();
                if crate::session::is_safe_component(&v) {
                    return v;
                }
            }
        }
        i += 1;
    }
    "default".to_string()
}

/// Try-parse the CLI args, turning usage errors into a JSON error object. `--help` and
/// `--version` are protocol exceptions and exit 0 here (Clap prints them).
pub fn parse_args_fallback() -> Args {
    use clap::Parser;
    match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                std::process::exit(0);
            }
            let session = crate::cli::session_hint();
            let _ = e;
            println!(
                "{}",
                serde_json::json!({
                    "session_id": session,
                    "status": "error",
                    "error": { "code": "usage", "message": "invalid arguments" }
                })
            );
            std::process::exit(2);
        }
    }
}

/// Error payload used by `main`.
pub struct AppError {
    pub code: Code,
    pub key: &'static str,
    pub message: String,
}

impl AppError {
    /// Build an error whose message has passed the **final** scrub-and-UTF8-bound for
    /// user-facing output: every surfaced diagnostic is at most
    /// [`crate::redact::MAX_DIAGNOSTIC_BYTES`] including the marker, so an
    /// oversized persisted scalar interpolated into a message can never blow the stdout
    /// output field.
    pub fn new(code: Code, key: &'static str, message: impl Into<String>) -> Self {
        AppError {
            code,
            key,
            message: crate::redact::scrub_and_bound_diagnostic(&message.into()),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered = format!("{}: {}", self.key, self.message);
        f.write_str(&crate::redact::scrub_and_bound_diagnostic(&rendered))
    }
}

impl std::fmt::Debug for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for AppError {}
fn read_stdin_prompt() -> Result<String, AppError> {
    use std::io::Read;
    if std::io::stdin().is_terminal() {
        return Err(AppError::new(
            Code::Usage,
            "missing-prompt",
            "no prompt given; pass -p/--prompt or pipe text on stdin",
        ));
    }
    let mut buf = Vec::with_capacity(MAX_STDIN_BYTES.min(64 * 1024));
    std::io::stdin()
        .take(MAX_STDIN_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| AppError::new(Code::Usage, "stdin", format!("read stdin: {e}")))?;
    if buf.len() > MAX_STDIN_BYTES {
        return Err(AppError::new(
            Code::Usage,
            "stdin-too-large",
            "stdin prompt exceeds the byte limit",
        ));
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(s),
        Err(_) => Err(AppError::new(
            Code::Usage,
            "stdin",
            "stdin is not valid UTF-8",
        )),
    }
}

fn resolve_profile(args: &Args, config_root: &std::path::Path) -> Result<Profile, AppError> {
    match &args.profile_load {
        Some(path) => {
            Profile::load_file(path).map_err(|e| AppError::new(Code::Config, "profile-load", e))
        }
        None => {
            let name = args.profile.as_deref().unwrap_or("dsflash-mi300x");
            match ProfileResolver.load_in(name, config_root) {
                Ok(ResolveOutcome::Loaded(p)) => Ok(*p),
                Ok(ResolveOutcome::Missing(name)) => Err(AppError::new(
                    Code::Config,
                    "profile-missing",
                    format!("profile {name:?} not found in the llxprt-code profiles dir"),
                )),
                Err(e) => Err(AppError::new(Code::Config, "profile-load", e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod turn_time_tests {
    use super::parse_turn_time;
    use std::time::Duration;

    #[test]
    fn turn_time_accepts_unit_durations() {
        assert_eq!(parse_turn_time("90s"), Ok(Some(Duration::from_secs(90))));
        assert_eq!(parse_turn_time("30m"), Ok(Some(Duration::from_secs(1800))));
        assert_eq!(parse_turn_time("2h"), Ok(Some(Duration::from_secs(7200))));
    }

    #[test]
    fn turn_time_zero_and_absent_disable() {
        assert_eq!(parse_turn_time("0"), Ok(None));
        assert_eq!(parse_turn_time("0s"), Ok(None));
        assert_eq!(parse_turn_time("0h"), Ok(None));
    }

    #[test]
    fn turn_time_rejects_bad_grammar() {
        for raw in ["30", "-5m", "m", "1.5h", "1d", "1x", "", "  "] {
            assert!(parse_turn_time(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn turn_time_rejects_overflow() {
        assert!(parse_turn_time((u64::MAX.to_string() + "h").as_str()).is_err());
    }
}
