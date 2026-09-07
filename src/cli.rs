//! Command-line entry point and JSON stdout contract.
//!
//! Every runtime outcome — including failures — emits exactly one JSON object on stdout
//! with a `session_id`. `--help`/`--version` are protocol exceptions (Clap renders
//! them and exits 0). All other outcomes (including Clap usage errors) are exactly one
//! JSON object.

use crate::agent::CodingAgent;
use crate::envelope::{Envelope, OkEnvelope};
use crate::model::{ProfileResolver, ResolveOutcome};
use crate::model_api::dependencies::RuntimeDependencies;
use crate::model_api::registry::construct_backend;
use crate::profile::{Profile, RedactedUrl};
use crate::session::load_session_store_in;
use crate::session::SessionId;
use crate::settings::Settings;
use clap::Parser;
use std::io::IsTerminal;
use std::path::PathBuf;

mod run;
pub use run::run_profiled;

/// Re-exported exit code type (defined in the leaf `envelope` module).
pub use crate::envelope::Code;

/// The maximum number of bytes read from stdin before the prompt is rejected. The cap is
/// applied **while reading**, not after allocation.
const MAX_STDIN_BYTES: usize = crate::session::MAX_PROMPT_BYTES;

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

    /// Stream phase-sampled process RSS events to a create-only JSONL file (default off).
    #[arg(long, value_name = "PATH")]
    pub mem_profile: Option<PathBuf>,

    /// Explicit opt-in to allow plaintext HTTP to a remote host (dsflash-mi300x style).
    #[arg(long)]
    pub allow_insecure_http: bool,

    /// Explicit opt-in to register the run_shell_command tool.
    #[arg(long)]
    pub allow_shell: bool,

    /// Per-prompt tool-call budget: `1..=512`, or `-1` for unlimited. Overrides the
    /// profile's `maxToolCallsPerPrompt`; when omitted, the profile field applies.
    /// The built-in default is 256 (half the ceiling).
    #[arg(long, value_name = "N")]
    pub max_tool_calls: Option<i64>,

    /// Wall-clock budget per prompt like `90s`, `30m`, `2h`; `0` disables.
    /// Omitted means no time limit.
    #[arg(long, value_name = "DURATION")]
    pub turn_time: Option<String>,

    /// Cap on one shell result in bytes (default 32768).
    #[arg(long, value_name = "BYTES")]
    pub max_shell_output: Option<u64>,

    /// Cap on one tool result in bytes (default 16777216).
    #[arg(long, value_name = "BYTES")]
    pub max_tool_output: Option<u64>,

    /// Cap on the combined tool-result bytes of one turn (default 16777216).
    #[arg(long, value_name = "BYTES")]
    pub max_turn_output: Option<u64>,

    /// How `modelParams` keys this build does not itself type are handled:
    /// `loose` (default) forwards them verbatim on the provider wire,
    /// `known-model` checks them against the checked-in model registry at load,
    /// `strict` refuses them at load.
    #[arg(long, value_name = "MODE")]
    pub model_params_mode: Option<crate::settings::ModelParamsMode>,

    /// Provider request timeout: seconds as an integer or a duration string like
    /// `90s`, `30m`, `2h`, `1500ms`. Defaults to 900 seconds for every
    /// provider. Overrides the settings file and the profile's provider `timeoutMs`.
    #[arg(long, value_name = "DURATION")]
    pub request_timeout: Option<String>,

    /// Print the resolved layered settings JSON and exit without constructing a backend.
    #[arg(long)]
    pub print_config: bool,
}

/// Resolve the actual configuration layers used both by runtime and `--print-config`.
pub(crate) fn resolve_settings(args: &Args) -> Result<Settings, AppError> {
    use crate::settings::{self, SettingsBudgets, SettingsLayer, SettingsLayers, SettingsProvider};

    let root = crate::config::std_profile_dir()
        .map_err(|e| AppError::new(Code::Config, "config-home", e))?;
    let profile = resolve_profile(args, &root)?;
    let profile_max = match profile.ephemeral.max_tool_calls_per_prompt {
        crate::profile::MaxToolCalls::Limited(value) => i64::try_from(value).ok(),
        crate::profile::MaxToolCalls::Unset | crate::profile::MaxToolCalls::Unlimited => None,
    };
    let profile_path = args.profile_load.clone().or_else(|| {
        args.profile
            .as_ref()
            .map(|name| root.join("profiles").join(format!("{name}.json")))
    });
    let env = settings::environment_layer()
        .map_err(|e| AppError::new(Code::Config, "settings-resolve", e))?;
    let cli = SettingsLayer {
        provider: SettingsProvider {
            model_params_mode: args.model_params_mode.map(|mode| mode.as_str().to_string()),
            ..Default::default()
        },
        budgets: SettingsBudgets {
            max_tool_calls: args.max_tool_calls,
            turn_time: args.turn_time.clone(),
            max_shell_output: args.max_shell_output,
            max_tool_output: args.max_tool_output,
            max_turn_output: args.max_turn_output,
            request_timeout: args.request_timeout.clone(),
        },
        ..Default::default()
    };
    let profile = SettingsLayer {
        provider: SettingsProvider {
            base_url: profile
                .ephemeral
                .base_url
                .as_ref()
                .map(|url| url.full().to_string()),
            model: Some(profile.model),
            profile_path,
            model_params_mode: None,
        },
        budgets: SettingsBudgets {
            max_tool_calls: profile_max,
            request_timeout: profile.ephemeral.timeout_ms.map(|ms| format!("{ms}ms")),
            ..Default::default()
        },
        ..Default::default()
    };
    let user_file = settings::load_user_file(&root)
        .map_err(|e| AppError::new(Code::Config, "settings-load", e))?;
    settings::resolve(SettingsLayers {
        user_file,
        profile,
        env,
        cli,
        config_root: root,
    })
    .map_err(|e| AppError::new(Code::Config, "settings-resolve", e.to_string()))
}

/// Render the resolved settings without constructing a backend.
pub fn print_config(args: &Args) -> Result<String, AppError> {
    serde_json::to_string(&resolve_settings(args)?)
        .map_err(|e| AppError::new(Code::Config, "settings-json", e.to_string()))
}

/// Apply the resolver's provider values to the selected profile before backend construction.
pub(crate) fn apply_runtime_settings(
    profile: &mut Profile,
    settings: &Settings,
) -> Result<(), AppError> {
    profile.model.clone_from(&settings.provider.model.value);
    profile.ephemeral.base_url = Some(
        RedactedUrl::parse(&settings.provider.base_url.value)
            .map_err(|e| AppError::new(Code::Config, "settings-resolve", e))?,
    );
    // The single resolved request-timeout policy is written onto the profile as provider
    // data; every provider backend consumes `profile.ephemeral.timeout_ms`.
    profile.ephemeral.timeout_ms = Some(settings.budgets.request_timeout.value.as_millis() as u64);
    Ok(())
}
/// Outcome of a successful invocation.
pub struct RunOutcome {
    pub session: SessionId,
    pub session_dir: std::path::PathBuf,
    pub run: crate::agent::CompletedRun,
    /// Resolved output caps (issue 77): per-result shell/tool caps and the aggregate
    /// per-turn tool-output cap, reported once per run.
    pub output_caps: crate::agent::OutputCaps,
}

/// Run the full workflow from parsed args.
pub fn run(args: Args) -> Result<RunOutcome, AppError> {
    run_profiled(args, None)
}

/// Record an optional memory-profile boundary.
fn profile_event(
    profiler: &Option<crate::memory_profile::Profiler>,
    phase: &'static str,
    data: crate::memory_profile::EventData,
) -> Result<(), AppError> {
    match profiler {
        Some(profiler) => profiler.event(phase, data).map_err(AppError::profiling),
        None => Ok(()),
    }
}

/// Durable context facts re-read from the session directory after the run.
struct ContextDeclaration {
    terminal_outcome: Option<String>,
    preserved: Vec<String>,
}

/// Best-effort re-read of `context/manifest.json`, published by the session store.
///
/// The agent owns the run; the envelope only reports what the store already committed,
/// so a missing or unreadable manifest changes nothing about the reported outcome.
fn context_declaration(session_dir: &std::path::Path) -> ContextDeclaration {
    let mut declared = ContextDeclaration {
        terminal_outcome: None,
        preserved: Vec::new(),
    };
    // Both readable locations are consulted independently: the manifest keeps
    // supplying `preserved`, while the best-effort marker the store drops beside
    // the session when only `context/` is unwritable wins for `quiesce`.
    let manifest = std::fs::read_to_string(
        crate::session::context_artifact_dir(session_dir).join("manifest.json"),
    )
    .ok()
    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let marker = std::fs::read_to_string(session_dir.join("context-quiesce.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    if let Some(value) = &marker {
        if let Some(outcome) = value.get("quiesce").and_then(serde_json::Value::as_str) {
            if !outcome.is_empty() {
                declared.terminal_outcome = Some(outcome.to_string());
            }
        }
    }
    // Without a marker the manifest's own quiesce verdict still counts.
    if declared.terminal_outcome.is_none() {
        if let Some(value) = &manifest {
            if let Some(outcome) = value.get("quiesce").and_then(serde_json::Value::as_str) {
                if !outcome.is_empty() {
                    declared.terminal_outcome = Some(outcome.to_string());
                }
            }
        }
    }
    // A normal policy terminal is considered only after both quiesce locations.
    if declared.terminal_outcome.is_none() {
        for value in [&marker, &manifest].into_iter().flatten() {
            if let Some(outcome) = value
                .get("terminal_outcome")
                .and_then(serde_json::Value::as_str)
            {
                if !outcome.is_empty() {
                    declared.terminal_outcome = Some(outcome.to_string());
                    break;
                }
            }
        }
    }
    let preserved_from = manifest.as_ref().or(marker.as_ref());
    if let Some(value) = preserved_from {
        if let Some(spans) = value.get("preserved").and_then(serde_json::Value::as_array) {
            declared.preserved = spans
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect();
        }
    }
    declared
}

/// Construct a typed outcome envelope with the final session identity.
///
/// `error_session_id` supplies identity only when `outcome` is an error. For successful
/// outcomes a debug-only assertion checks that it agrees with [`RunOutcome`]'s validated session.
pub fn envelope(outcome: &Result<RunOutcome, AppError>, error_session_id: &str) -> Envelope {
    match outcome {
        Ok(o) => {
            debug_assert_eq!(error_session_id, o.session.id);
            let declared = context_declaration(&o.session_dir);
            let summary = if declared.preserved.is_empty() {
                o.run.summary.clone()
            } else {
                let spans = declared.preserved.join(" / ");
                format!("{} | preserved: {spans}", o.run.summary)
            };
            Envelope::Ok(OkEnvelope {
                session_id: o.session.id.clone(),
                session_dir: o.session_dir.display().to_string(),
                turn: u64::from(o.run.turn),
                attempt: u64::from(o.run.attempt),
                branch_id: o.run.branch_id.clone(),
                branch: o.run.branch,
                replayed: o.run.replayed,
                summary,
                tool_calls: u64::try_from(o.run.tool_count).unwrap_or(u64::MAX),
                declared_tool_calls: o
                    .run
                    .declared_tool_calls
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(-1),
                budget_exhausted: o.run.budget_exhausted,
                zero_call_tail: o.run.zero_call_tail,
                prompt_digest: o.run.prompt_digest.clone(),
                output_caps: o.output_caps,
                terminal_outcome: declared.terminal_outcome,
            })
        }
        Err(error) if error.code == Code::Profiling => Envelope::profiling_error(
            error_session_id,
            error.message.clone(),
            error.profiling_stage.unwrap_or("sample"),
            error.session_status.as_deref().unwrap_or("ok"),
        ),
        Err(error) => {
            let mut envelope = Envelope::error(
                error_session_id,
                error.envelope_code.unwrap_or(error.key),
                error.message.clone(),
            );
            if let Envelope::Error(detail) = &mut envelope {
                if let Some(outcome) = error.terminal_outcome {
                    detail.error.terminal_outcome = Some(outcome.to_string());
                }
            }
            envelope
        }
    }
}

/// Serialize the typed outcome through `Value` to preserve historical sorted-key bytes.
pub fn json(outcome: &Result<RunOutcome, AppError>, session_id: &str) -> serde_json::Value {
    envelope(outcome, session_id).to_value()
}

/// Exit code for an outcome.
pub fn exit_code(outcome: &Result<RunOutcome, AppError>) -> i32 {
    match outcome {
        Ok(_) => 0,
        Err(e) => e.code as i32,
    }
}

/// Best-effort session id for error payloads: the validated `--session` value in argv.
pub fn session_hint() -> String {
    session_hint_from(std::env::args_os().skip(1))
}

fn session_hint_from(args: impl IntoIterator<Item = std::ffi::OsString>) -> String {
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].to_string_lossy();
        if raw == "--" {
            break;
        }
        if let Some(value) = raw.strip_prefix("--session=") {
            if crate::session::is_safe_component(value) {
                return value.to_string();
            }
        } else if raw == "--session" {
            if let Some(value) = args.get(i + 1) {
                let value = value.to_string_lossy();
                if crate::session::is_safe_component(&value) {
                    return value.into_owned();
                }
            }
        }
        i += 1;
    }
    SessionId::fresh().id
}

fn has_session_argument(args: &[std::ffi::OsString]) -> bool {
    args.iter()
        .take_while(|argument| *argument != "--")
        .any(|argument| {
            let argument = argument.to_string_lossy();
            argument == "--session" || argument.starts_with("--session=")
        })
}

/// Try-parse the CLI args, turning usage errors into a JSON error object. `--help` and
/// `--version` are protocol exceptions and exit 0 here (Clap prints them).
pub fn parse_args_fallback(session_hint: &str) -> Args {
    use clap::Parser;
    let mut argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if !has_session_argument(&argv[1..]) {
        argv.splice(
            1..1,
            [std::ffi::OsString::from("--session"), session_hint.into()],
        );
    }
    match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                std::process::exit(0);
            }
            let _ = e;
            print!(
                "{}",
                String::from_utf8_lossy(
                    &Envelope::error(session_hint, "usage", "invalid arguments").to_line()
                )
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
    pub profiling_stage: Option<&'static str>,
    pub session_status: Option<String>,
    /// The stable envelope `error.code` token. `None` means [`Self::key`] is already the
    /// envelope token. A model transport failure carries its finer `model-<class>`
    /// transport key here (for example `model-quota-exhausted`) while [`Self::key`] stays
    /// `model`, so the process exit code family is unchanged.
    pub envelope_code: Option<&'static str>,
    /// Terminal outcome the run declared for itself (issue 146): a collapsed turn the
    /// caller must be able to distinguish from a finished one.
    pub terminal_outcome: Option<&'static str>,
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
            profiling_stage: None,
            session_status: None,
            envelope_code: None,
            terminal_outcome: None,
        }
    }

    /// Carry a finer envelope `error.code` token than [`Self::key`], leaving the process
    /// exit code family alone.
    pub fn with_envelope_code(mut self, envelope_code: Option<&'static str>) -> Self {
        self.envelope_code = envelope_code;
        self
    }

    pub fn profiling(error: crate::memory_profile::ProfilingError) -> Self {
        Self::profiling_at(error.stage, error.message)
    }

    pub fn profiling_at(stage: &'static str, message: impl Into<String>) -> Self {
        AppError {
            code: Code::Profiling,
            key: "mem-profile",
            message: crate::redact::scrub_and_bound_diagnostic(&message.into()),
            profiling_stage: Some(stage),
            session_status: Some("ok".into()),
            envelope_code: None,
            terminal_outcome: None,
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
    use crate::settings::parse_turn_time;
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

#[cfg(test)]
mod tests {
    use super::{has_session_argument, session_hint_from, Args};
    use clap::Parser as _;
    use std::ffi::OsString;

    #[test]
    fn omitted_session_gets_a_fresh_valid_hint() {
        let first = session_hint_from(Vec::<OsString>::new());
        let second = session_hint_from(Vec::<OsString>::new());
        assert_ne!(first, second);
        assert!(crate::session::SessionId::parse(&first).is_ok());
        assert!(crate::session::SessionId::parse(&second).is_ok());
    }

    #[test]
    fn explicit_default_is_preserved() {
        let arguments = vec![OsString::from("--session"), OsString::from("default")];
        assert_eq!(session_hint_from(arguments.clone()), "default");
        assert!(has_session_argument(&arguments));

        let args = Args::try_parse_from(["llxprt-code-rs", "--session", "default"]).unwrap();
        assert_eq!(args.session, "default");
    }

    #[test]
    fn session_arguments_after_end_of_options_are_not_options() {
        for arguments in [
            vec![
                OsString::from("--"),
                OsString::from("--session"),
                OsString::from("named"),
            ],
            vec![OsString::from("--"), OsString::from("--session=named")],
        ] {
            assert!(!has_session_argument(&arguments));
        }
    }

    #[test]
    fn invalid_explicit_session_does_not_leak_into_error_envelope_hint() {
        let arguments = vec![OsString::from("--session=../escape")];
        let hint = session_hint_from(arguments);
        assert_ne!(hint, "../escape");
        assert!(crate::session::SessionId::parse(&hint).is_ok());
    }

    #[test]
    fn print_config_parses_the_output_cap_flags() {
        let args = Args::try_parse_from([
            "llxprt-code-rs",
            "--print-config",
            "--max-shell-output",
            "1048576",
            "--max-tool-output",
            "1048576",
            "--max-turn-output",
            "20971520",
        ])
        .unwrap();
        assert!(
            args.print_config,
            "main dispatches this flag before profiler/backend setup"
        );
        assert_eq!(args.max_shell_output, Some(1048576));
        assert_eq!(args.max_tool_output, Some(1048576));
        assert_eq!(args.max_turn_output, Some(20971520));
    }

    #[test]
    fn print_config_resolves_and_exits_before_backend() {
        let args = Args::try_parse_from(["llxprt-code-rs", "--print-config"]).unwrap();
        assert!(
            args.print_config,
            "main dispatches this flag before profiler/backend setup"
        );
    }
}
