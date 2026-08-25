//! Black-box evaluation harness for the `llxprt-code-rs` CLI against a live model.
//!
//! The harness spawns the real binary as a subprocess and parses its stdout strictly as
//! exactly one JSON object (typed extraction, not line sniffing). For `dsflash`
//! scenarios it passes the explicit `--allow-insecure-http` and `--allow-shell`
//! opt-ins, because that named profile targets a remote plaintext HTTP endpoint and enables
//! the shell tool on purpose.
//!
//! Follow-up turns share the same session and workspace. After the first failed turn no
//! further turn is attempted ([`run_turns`] aborts).
//!
//! The captured stdout/stderr are raw bytes ([`BbResult::raw_stdout`],
//! [`BbResult::stderr`]) with per-stream truncation flags from the bounded runner. Only a
//! separate UTF-8 slice is decoded for the strict single-JSON parse; the raw bytes (which
//! may not be valid UTF-8) are preserved verbatim for artifact files.
//!
//! Nothing here talks to the network itself; network traffic comes only from the spawned
//! CLI, which reads the same profile and keyfile that regular llxprt-code uses
//! (default profile: `dsflash-mi300x`).

mod inventory;
#[cfg(test)]
use inventory::inventory_inner;
pub use inventory::{
    inventory, inventory_cap, is_regular_no_follow, score_present, score_present_cap,
};
mod save;
#[cfg(test)]
use save::ensure_artifact_subdir;
pub use save::save_turn;

use crate::process::{self, CmdOutcome, CmdSpec};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where harness progress goes. The CLI must keep stdout clean, so the harness echoes the
/// CLI's stderr and its own progress on stderr. Diagnostics are always printed (a
/// non-terminal stderr is still where a script or CI watches).
pub fn eprint_status(msg: &str) {
    eprintln!("{msg}");
}

/// Cap on how many inventory entries a scenario report carries; beyond that the inventory
/// records `truncated` instead of silently dropping the tail.
pub const MAX_INVENTORY_ITEMS: usize = 2000;
/// Cap on the cumulative path bytes an inventory may carry before traversal stops.
pub const MAX_INVENTORY_BYTES: usize = 1 << 20;
/// Cap on how deep an inventory descends before traversal stops.
pub const MAX_INVENTORY_DEPTH: usize = 32;

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub files: Vec<String>,
    pub truncated: bool,
}

/// What one spawned CLI process produced. `raw_stdout`/`stderr` keep the exact
/// subprocess bytes verbatim (they may not be valid UTF-8); the truncation flags come
/// from the bounded runner so an artifact records whether bytes were dropped.
#[derive(Debug, Clone)]
pub struct BbResult {
    /// Parse-consistent success: the CLI reported `"status":"ok"` with every required
    /// envelope field, the requested session and expected turn, and exited 0.
    pub ok: bool,
    /// The parsed `status` field (or `"spawn-failed"` / `"stdout-contract-broken"`).
    pub status: String,
    /// The subprocess exit code, or `None` when the process was killed by a signal.
    pub exit: Option<i32>,
    /// Whether the bounded runner let the run hit its deadline (a timed-out run whose
    /// output was cut short can never be a protocol pass).
    pub timed_out: bool,
    pub session_id: String,
    /// The (validated) turn number the CLI reported.
    pub turn: u32,
    pub attempt: u32,
    pub branch_id: String,
    pub branch: bool,
    pub replayed: bool,
    /// The CLI's own executed tool-call count for the turn (from the validated envelope).
    pub tool_calls: usize,
    pub prompt_digest: String,
    pub summary: String,
    pub error_code: String,
    pub error_message: String,
    /// Exact stdout bytes (untrimmed, verbatim).
    pub raw_stdout: Vec<u8>,
    /// Exact stderr bytes (untrimmed, verbatim).
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub combined_truncated: bool,
}

impl BbResult {
    /// A placeholder for an invocation that could not be spawned (no stdout yet).
    fn failed_spawn(error_message: String) -> BbResult {
        BbResult {
            ok: false,
            status: "spawn-failed".into(),
            exit: None,
            timed_out: false,
            session_id: String::new(),
            turn: 0,
            attempt: 0,
            branch_id: String::new(),
            branch: false,
            replayed: false,
            tool_calls: 0,
            prompt_digest: String::new(),
            summary: String::new(),
            error_code: "spawn".into(),
            error_message,
            raw_stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            combined_truncated: false,
        }
    }
}

/// The CLI's exactly-one-JSON-envelope shape, discriminated on `status` into a success
/// or error envelope. Every status-specific struct is `#[serde(deny_unknown_fields)]`:
/// a success carrying a field outside its contract (including an `error` object, an extra
/// `exit`, or any typo) fails the typed parse, and so does an error carrying success
/// fields. The `status` key selects the variant and is not a struct field.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Envelope {
    Ok(OkEnvelope),
    Error(ErrorEnvelope),
}

/// The required success envelope. Every field listed in the contract is present: the exact
/// session, a non-empty on-disk session dir, the expected turn, a 1-based attempt, a
/// non-empty branch id, the branch/replayed flags, summary, the executed tool-call count,
/// and the prompt digest. The `status` key selects this variant and is consumed by the tag;
/// any other field (`error`, an extra `exit`, a typo) is rejected by
/// `deny_unknown_fields`, so a success never carries an `error`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OkEnvelope {
    pub session_id: String,
    pub session_dir: String,
    pub turn: u64,
    pub attempt: u64,
    pub branch_id: String,
    pub branch: bool,
    pub replayed: bool,
    pub summary: String,
    pub tool_calls: u64,
    pub prompt_digest: String,
}

/// The required error envelope: the success-optional `session_id` and the error detail are
/// permitted; the `status` key selects this variant and is consumed by the tag; success
/// fields (turn, attempt, summary, tool_calls, ...) are rejected by
/// `deny_unknown_fields`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    pub session_id: Option<String>,
    pub error: EnvelopeError,
}

/// The error detail carried by an error envelope; no success field may be present.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeError {
    pub code: String,
    pub message: String,
}

/// One CLI invocation: argv pieces plus a fresh workspace dir, unique session id, and prompt.
#[derive(Debug, Clone)]
pub struct InvocationSpec {
    pub session: String,
    pub cwd: PathBuf,
    pub prompt: String,
    /// 1-based turn number; `None` is the first turn (turn 1).
    pub turn: Option<u32>,
    /// Parent branch id to continue from (passed through as `--branch <id>`). The
    /// successful child reports its own distinct `branch_id`.
    pub branch: Option<String>,
    pub profile: Option<String>,
    /// Pass `--allow-insecure-http` (required for a non-loopback plaintext HTTP profile).
    pub allow_insecure_http: bool,
    /// Pass `--allow-shell` (registers the `run_shell_command` tool).
    pub allow_shell: bool,
}

impl InvocationSpec {
    /// Build the exact argv the CLI is spawned with, honoring the explicit opt-ins.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--session".to_string(),
            self.session.clone(),
            "--cwd".to_string(),
            self.cwd.display().to_string(),
            "-p".to_string(),
            self.prompt.clone(),
        ];
        if let Some(t) = self.turn {
            args.push("--turn".to_string());
            args.push(t.to_string());
        }
        if let Some(b) = &self.branch {
            args.push("--branch".to_string());
            args.push(b.clone());
        }
        if let Some(p) = &self.profile {
            args.push("--profile".to_string());
            args.push(p.clone());
        }
        if self.allow_insecure_http {
            args.push("--allow-insecure-http".to_string());
        }
        if self.allow_shell {
            args.push("--allow-shell".to_string());
        }
        args
    }
}

/// Exactly-one-object typed parse on a **separate UTF-8 slice** of the raw stdout:
/// anything that is not a single JSON value (including trailing content after the object, a
/// second value, or invalid UTF-8) is a contract failure regardless of the model result.
fn parse_one_object(stdout: &[u8]) -> Result<Envelope, String> {
    let text =
        std::str::from_utf8(stdout).map_err(|e| format!("stdout is not valid UTF-8: {e}"))?;
    if text.trim().is_empty() {
        return Err("empty stdout, expected one JSON object".into());
    }
    serde_json::from_str::<Envelope>(text)
        .map_err(|e| format!("stdout is not exactly one typed JSON object: {e}"))
}

/// Unique id suffix (wall-clock nanos; not a secret).
pub fn uniq() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

fn create_private_dir(root: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    loop {
        let dir = root.join(format!("{prefix}-{}", uniq()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

/// Fresh private directory under the system temporary directory, created atomically.
pub(crate) fn fresh_private_dir(prefix: &str) -> std::io::Result<PathBuf> {
    create_private_dir(&std::env::temp_dir(), prefix)
}

/// Fresh isolated workspace under `root`, guaranteed not to already exist.
pub fn fresh_workspace(root: &Path) -> PathBuf {
    create_private_dir(root, "ws")
        .unwrap_or_else(|error| panic!("create workspace under {}: {error}", root.display()))
}

/// Spawn the real CLI and parse its stdout. The `LLXPRT_CODE_RS_BIN` env var is
/// honoured first so the parity binary can be driven from the shell; as a fallback the
/// `cargo test`-baked path is used. The runner passes through only PATH, HOME, TMPDIR,
/// LANG, LC_*, CARGO_HOME, RUSTUP_HOME, and RUSTUP_TOOLCHAIN.
pub fn cli_binary() -> String {
    if let Ok(p) = std::env::var("LLXPRT_CODE_RS_BIN") {
        return p;
    }
    option_env!("CARGO_BIN_EXE_llxprt-code-rs")
        .unwrap_or("llxprt-code-rs")
        .to_string()
}

/// The config-dir selectors that must reach the child (so a parity run under
/// `LLXPRT_CONFIG_HOME`/`LLXPRT_CONFIG_DIR` uses the same profiles and sessions).
/// Unrelated env — including any credential `*KEY*`/`*TOKEN*`/`*SECRET*`
/// variables in the parent — is not part of this list; the runner's allow-list never
/// passes it through.
fn config_env_add() -> Vec<(String, String)> {
    let mut env_add = Vec::new();
    for key in ["LLXPRT_CONFIG_HOME", "LLXPRT_CONFIG_DIR"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                env_add.push((key.to_string(), v));
            }
        }
    }
    env_add
}

/// Spawn the real CLI and parse its stdout (strictly one JSON object). The flags from
/// [`InvocationSpec::to_args`] are passed through verbatim, including the explicit
/// opt-ins.
pub fn cli_command(spec: &InvocationSpec) -> Result<CmdOutcome, String> {
    let specs = InvocationSpec {
        session: spec.session.clone(),
        cwd: spec.cwd.clone(),
        prompt: spec.prompt.clone(),
        turn: spec.turn,
        branch: spec.branch.clone(),
        profile: spec.profile.clone(),
        allow_insecure_http: spec.allow_insecure_http,
        allow_shell: spec.allow_shell,
    };
    process::run_cmd(CmdSpec {
        program: cli_binary(),
        args: specs.to_args(),
        cwd: None,
        cwd_fd: None,
        env_add: config_env_add(),
        timeout: Duration::from_secs(900),
        max_output: 32 * 1024 * 1024,
    })
}

/// Cross-turn continuation state one parity run accumulates, so every turn after the first
/// is checked against the session identity/path and turn/progress semantics seen so
/// far. The state lives for the whole [`run_turns`] drive (not per invocation), so a
/// changed session id, a directory pointing at a different identity, or a first-time turn
/// claiming a replayed/retried/branched shape is rejected, while a legitimate
/// retry/branch the harness explicitly requested (or a re-run it drives) stays possible.
#[derive(Default)]
pub struct ContinuationState {
    /// The session identity and its directory path from the first validated turn.
    pub session_dir: Option<String>,
    /// Every `(turn, prompt_digest)` this drive has already seen completed.
    pub completed: Vec<(u32, String)>,
    /// Every branch id returned by a successfully validated turn in this drive.
    pub branch_ids: Vec<String>,
}

/// Run one CLI turn and build the typed [`BbResult] against cross-turn continuation
/// state. The exact stdout/stderr bytes from the subprocess are preserved on
/// [`BbResult`] verbatim, and the envelope is validated strictly: the requested
/// session, expected turn, required per-status fields, the exit/status agreement, and the
/// semantically possible first/continuation metadata.
pub fn run_cli_with_state(spec: InvocationSpec, state: &mut ContinuationState) -> BbResult {
    let out = match cli_command(&spec) {
        Ok(o) => o,
        Err(e) => return BbResult::failed_spawn(e),
    };
    let mut result = BbResult {
        ok: false,
        status: "stdout-contract-broken".into(),
        exit: out.status,
        timed_out: out.timed_out,
        session_id: String::new(),
        turn: 0,
        attempt: 0,
        branch_id: String::new(),
        branch: false,
        replayed: false,
        tool_calls: 0,
        prompt_digest: String::new(),
        summary: String::new(),
        error_code: String::new(),
        error_message: String::new(),
        raw_stdout: out.stdout.clone(),
        stderr: out.stderr.clone(),
        stdout_truncated: out.stdout_truncated,
        stderr_truncated: out.stderr_truncated,
        combined_truncated: out.combined_truncated,
    };
    match parse_one_object(&out.stdout) {
        Ok(env) => {
            if let Err(e) = fill(&mut result, &env, &spec, state) {
                result.error_message = e;
            }
        }
        Err(e) => result.error_message = e,
    }
    result
}

/// Run one CLI turn and build the typed [`BbResult`] (a single-turn view with fresh
/// continuation state, used by callers that validate one invocation at a time; the parity
/// binary drives multi-turn runs through [`run_cli_with_state`]).
pub fn run_cli(spec: InvocationSpec) -> BbResult {
    run_cli_with_state(spec, &mut ContinuationState::default())
}

/// Validate the envelope against the requested invocation and fill the typed [`BbResult`].
///
/// A success (`Envelope::Ok`) must match the requested session and expected turn, report a
/// 1-based attempt, non-empty `session_dir`/`branch_id`/`prompt_digest`, a
/// `prompt_digest` equal to the same FNV-1a digest applied independently to the
/// submitted prompt, a previously observed parent when `--branch` was given, and must
/// have exited 0. An error (`Envelope::Error`) must carry its error detail and
/// must have exited nonzero. Any envelope outside these contracts leaves `ok = false`.
fn fill(
    result: &mut BbResult,
    env: &Envelope,
    spec: &InvocationSpec,
    state: &mut ContinuationState,
) -> Result<(), String> {
    match env {
        Envelope::Ok(env) => fill_ok(result, env, spec, state),
        Envelope::Error(env) => fill_error(result, env),
    }
}

/// Validate a success envelope against the request.
fn fill_ok(
    result: &mut BbResult,
    env: &OkEnvelope,
    spec: &InvocationSpec,
    state: &mut ContinuationState,
) -> Result<(), String> {
    result.status = "ok".to_string();
    validate_complete_output(result)?;
    let (turn, attempt) = validate_turn_identity(result, env, spec)?;
    validate_session_identity(env, state)?;
    validate_requested_parent(spec, state)?;
    validate_replay(env, spec, state, turn, attempt)?;
    validate_ok_fields(result, env, spec)?;
    state.completed.push((turn, env.prompt_digest.clone()));
    state.branch_ids.push(env.branch_id.clone());
    result.ok = true;
    Ok(())
}

fn validate_complete_output(result: &BbResult) -> Result<(), String> {
    let incomplete = result.stdout_truncated
        || result.combined_truncated
        || result.timed_out
        || result.exit.is_none();
    if incomplete {
        return Err(
            "status ok but the subprocess output was not fully captured (truncated, timed out, or killed by a signal)"
                .into(),
        );
    }
    Ok(())
}

fn validate_turn_identity(
    result: &mut BbResult,
    env: &OkEnvelope,
    spec: &InvocationSpec,
) -> Result<(u32, u32), String> {
    if env.session_id != spec.session {
        return Err(format!(
            "session_id mismatch: expected {}, got {}",
            spec.session, env.session_id
        ));
    }
    result.session_id = env.session_id.clone();
    let expected_turn = spec.turn.unwrap_or(1);
    let turn = u32::try_from(env.turn).map_err(|_| "turn out of range".to_string())?;
    if turn != expected_turn {
        return Err(format!("expected turn {expected_turn}, got {turn}"));
    }
    let attempt = u32::try_from(env.attempt).map_err(|_| "attempt out of range".to_string())?;
    if attempt < 1 {
        return Err(format!("attempt must be >= 1, got {attempt}"));
    }
    result.turn = turn;
    result.attempt = attempt;
    Ok((turn, attempt))
}

fn validate_session_identity(
    env: &OkEnvelope,
    state: &mut ContinuationState,
) -> Result<(), String> {
    if env.session_dir.trim().is_empty() {
        return Err("ok envelope has an empty session_dir".to_string());
    }
    let dir_tail = env
        .session_dir
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if dir_tail != env.session_id {
        return Err(format!(
            "session_dir does not identify the session: the directory is {env:?}"
        ));
    }
    match &state.session_dir {
        Some(first) if *first != env.session_dir => {
            Err("session path changed across turns of the same session".to_string())
        }
        Some(_) => Ok(()),
        None => {
            state.session_dir = Some(env.session_dir.clone());
            Ok(())
        }
    }
}
fn validate_requested_parent(
    spec: &InvocationSpec,
    state: &ContinuationState,
) -> Result<(), String> {
    let Some(parent) = spec.branch.as_deref() else {
        return Ok(());
    };
    if state.branch_ids.iter().any(|known| known == parent) {
        Ok(())
    } else {
        Err(format!(
            "CLI --branch parent {parent} was not returned by an earlier validated turn"
        ))
    }
}

fn validate_replay(
    env: &OkEnvelope,
    spec: &InvocationSpec,
    state: &ContinuationState,
    turn: u32,
    attempt: u32,
) -> Result<(), String> {
    let completed = state
        .completed
        .iter()
        .any(|(saved_turn, digest)| *saved_turn == turn && *digest == env.prompt_digest);
    if !completed && spec.branch.is_none() && attempt != 1 {
        return Err(format!(
            "attempt {attempt} is impossible on the first turn {turn} without an explicit --branch"
        ));
    }
    if !completed && spec.branch.is_none() && env.branch {
        return Err(format!(
            "branch=true is impossible on turn {turn} without an explicit --branch"
        ));
    }
    if !completed && env.replayed {
        return Err(format!(
            "replayed=true is impossible for turn {turn}: this prompt was never completed before"
        ));
    }
    if env.replayed && (!completed || env.branch) {
        return Err(
            "replayed is only accepted for a previously completed (turn, prompt)".to_string(),
        );
    }
    Ok(())
}

fn validate_ok_fields(
    result: &mut BbResult,
    env: &OkEnvelope,
    spec: &InvocationSpec,
) -> Result<(), String> {
    if env.branch_id.trim().is_empty() {
        return Err("ok envelope has an empty branch_id".to_string());
    }
    if env.prompt_digest.trim().is_empty() {
        return Err("ok envelope has an empty prompt_digest".to_string());
    }
    let want = crate::agent::prompt_digest(&spec.prompt);
    if env.prompt_digest != want {
        return Err(format!(
            "prompt_digest mismatch: expected {want}, got {}",
            env.prompt_digest
        ));
    }
    let tool_calls =
        usize::try_from(env.tool_calls).map_err(|_| "tool_calls out of range".to_string())?;
    if tool_calls > crate::agent::MAX_TOOL_CALLS_PER_TURN {
        return Err(format!(
            "tool_calls {tool_calls} exceeds the per-turn budget {} of one attempt",
            crate::agent::MAX_TOOL_CALLS_PER_TURN
        ));
    }
    if result.exit != Some(0) {
        return Err(format!(
            "status ok but exit code disagrees: {:?}",
            result.exit
        ));
    }
    result.branch_id = env.branch_id.clone();
    result.branch = env.branch;
    result.replayed = env.replayed;
    result.summary = env.summary.clone();
    result.tool_calls = tool_calls;
    result.prompt_digest = env.prompt_digest.clone();
    Ok(())
}
/// Validate an error envelope: it already parsed only if it carries its error detail and no
/// success field (the typed contract), and it must have exited nonzero.
fn fill_error(result: &mut BbResult, env: &ErrorEnvelope) -> Result<(), String> {
    result.status = "error".to_string();
    if let Some(id) = &env.session_id {
        result.session_id = id.clone();
    }
    result.error_code = env.error.code.clone();
    result.error_message = env.error.message.clone();
    // A failure must have exited nonzero (exit/status agreement).
    match result.exit {
        Some(code) if code != 0 => Ok(()),
        _ => Err(format!(
            "status error but exit code disagrees: {:?}",
            result.exit
        )),
    }
}

/// Drive every turn of a scenario, aborting on the first failure and never exceeding
/// `max_turns`. A turn that fails to even persist its artifacts propagates as `Err`.
pub fn run_turns<F>(scenario: &Scenario, mut run_turn: F) -> Result<Vec<BbResult>, String>
where
    F: FnMut(&str, Option<u32>) -> Result<BbResult, String>,
{
    let mut results = vec![run_turn(&scenario.prompt, None)?];
    let mut turn_n = 1u32;
    for follow in &scenario.follows {
        if turn_n >= scenario.max_turns as u32 {
            break;
        }
        if !results.last().map(|r| r.ok).unwrap_or(false) {
            break;
        }
        turn_n += 1;
        results.push(run_turn(follow, Some(turn_n))?);
    }
    Ok(results)
}

struct ArtifactCandidate {
    file: std::fs::File,
    len: u64,
    digest: [u8; 32],
}

fn stage_at(
    dir: &openat::Dir,
    final_name: &str,
    bytes: &[u8],
) -> Result<ArtifactCandidate, String> {
    use sha2::Digest as _;
    use std::io::Write as _;

    #[cfg(target_os = "linux")]
    let mut file = dir
        .new_unnamed_file(0o600)
        .map_err(|error| format!("create anonymous stage for {final_name}: {error}"))?;
    #[cfg(target_os = "macos")]
    let mut file = {
        let name = macos_candidate_name(final_name);
        let file = create_named_candidate_at(dir, &name)
            .map_err(|error| format!("create stage for {final_name}: {error}"))?;
        dir.remove_file(&name).map_err(|error| {
            format!("unlink retained stage for {final_name} before writing: {error}")
        })?;
        file
    };
    file.write_all(bytes)
        .map_err(|error| format!("write stage for {final_name}: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync stage for {final_name}: {error}"))?;
    Ok(ArtifactCandidate {
        file,
        len: bytes.len() as u64,
        digest: sha2::Sha256::digest(bytes).into(),
    })
}

fn publish_stage_at(
    dir: &openat::Dir,
    candidate: &ArtifactCandidate,
    final_name: &str,
) -> Result<(), String> {
    install_candidate_at(dir, candidate, final_name)
        .map_err(|error| format!("place {final_name}: {error}"))?;
    verify_candidate_at(dir, candidate, final_name)
        .map_err(|error| format!("verify installed {final_name}: {error}"))
}

/// A create-only artifact publication failure, distinguished by whether the final name was
/// installed before verification or directory durability failed.
#[derive(Debug)]
pub enum ArtifactPublishError {
    BeforePublication(String),
    InstalledDurabilityUnknown(String),
}

/// Publish one file through a retained parent-directory descriptor and an exact retained
/// candidate descriptor. The destination is create-only.
pub fn publish_create_only_file(path: &Path, bytes: &[u8]) -> Result<(), ArtifactPublishError> {
    let parent = path.parent().ok_or_else(|| {
        ArtifactPublishError::BeforePublication("artifact path has no parent".to_string())
    })?;
    let leaf = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ArtifactPublishError::BeforePublication("artifact name is not UTF-8".to_string())
        })?;
    fs::create_dir_all(parent)
        .map_err(|error| ArtifactPublishError::BeforePublication(error.to_string()))?;
    let dir = crate::tools::open_root(parent).map_err(ArtifactPublishError::BeforePublication)?;
    let candidate = stage_at(&dir, leaf, bytes).map_err(ArtifactPublishError::BeforePublication)?;
    install_candidate_at(&dir, &candidate, leaf).map_err(|error| {
        ArtifactPublishError::BeforePublication(format!("place {leaf}: {error}"))
    })?;
    verify_candidate_at(&dir, &candidate, leaf)
        .map_err(ArtifactPublishError::InstalledDurabilityUnknown)?;
    sync_artifact_dir(&dir)
        .map_err(|error| ArtifactPublishError::InstalledDurabilityUnknown(error.to_string()))?;
    verify_candidate_at(&dir, &candidate, leaf)
        .map_err(ArtifactPublishError::InstalledDurabilityUnknown)
}

fn verify_candidate_at(
    dir: &openat::Dir,
    candidate: &ArtifactCandidate,
    final_name: &str,
) -> Result<(), String> {
    use sha2::Digest as _;
    use std::io::Read as _;

    let mut installed = open_artifact_at(dir, final_name).map_err(|error| error.to_string())?;
    if installed
        .metadata()
        .map_err(|error| error.to_string())?
        .len()
        != candidate.len
    {
        return Err(format!(
            "installed artifact {final_name} has the wrong size"
        ));
    }
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = installed
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if digest != candidate.digest {
        return Err(format!(
            "installed artifact {final_name} has the wrong digest"
        ));
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn macos_candidate_name(final_name: &str) -> String {
    let mut random = [0u8; 16];
    unsafe {
        libc::arc4random_buf(random.as_mut_ptr().cast::<libc::c_void>(), random.len());
    }
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(".stage.{final_name}-{suffix}")
}

#[cfg(target_os = "macos")]
fn create_named_candidate_at(dir: &openat::Dir, name: &str) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let name = std::ffi::CString::new(name).unwrap();
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
}

#[cfg(target_os = "linux")]
fn install_candidate_at(
    dir: &openat::Dir,
    candidate: &ArtifactCandidate,
    final_name: &str,
) -> std::io::Result<()> {
    dir.link_file_at(&candidate.file, final_name)
}

#[cfg(target_os = "macos")]
fn install_candidate_at(
    dir: &openat::Dir,
    candidate: &ArtifactCandidate,
    final_name: &str,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let final_name = std::ffi::CString::new(final_name).unwrap();
    let result = unsafe {
        libc::fclonefileat(
            candidate.file.as_raw_fd(),
            dir.as_raw_fd(),
            final_name.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn open_artifact_at(dir: &openat::Dir, name: &str) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in name"))?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artifact is not a regular file",
        ));
    }
    Ok(file)
}

fn sync_artifact_dir(dir: &openat::Dir) -> std::io::Result<()> {
    dir.open_file(".")?.sync_all()
}

/// Cap on how many directory entries one inventory descent visits before stopping (the
/// inventory boundary), in addition to the item repository cap, so a hostile tree cannot
/// force an unbounded directory scan even when every descendant is a skipped special or
/// dot-path entry.
const MAX_INVENTORY_ENTRIES: usize = 200_000;

/// A scenario: a starter prompt plus optional follow-up turns, within a bounded budget.
#[derive(Clone)]
pub struct Scenario {
    pub name: String,
    pub prompt: String,
    pub max_turns: usize,
    pub follows: Vec<&'static str>,
}
/// The four parity scenarios. All four target the `dsflash-mi300x` profile, so the
/// explicit `--allow-insecure-http` and `--allow-shell` opt-ins are required. Each
/// may use an extra turn from `follows`; the harness never exceeds `max_turns`.
pub fn dsflash_scenario_spec(
    name: &str,
    prompt: String,
    max_turns: usize,
    follows: Vec<&'static str>,
) -> Scenario {
    Scenario {
        name: name.to_string(),
        prompt,
        max_turns,
        follows,
    }
}

/// The four canonical parity scenarios.
pub fn scenarios() -> Vec<Scenario> {
    vec![
        dsflash_scenario_spec(
            "starter",
            "Create a tiny Python project with two files: a module math_utils.py with an add(a,b) function, and a test file test_math_utils.py that verifies add(2,3)==5. Run the tests with python3 -m pytest or a plain python assert and report the result.".into(),
            2,
            vec!["In the same project add a module double.py with a function double(n) returning n*2 and a test that asserts double(21)==42 using a plain python assert, then run all checks."],
        ),
        dsflash_scenario_spec(
            "pong",
            "Create a tiny Pong game in Python with a STABLE headless API contract. Put ALL game logic in pong_logic.py with no GUI dependency and NO pygame import anywhere. pong_logic.py MUST define exactly these pure functions and top-level constants that an external checker imports and calls:
\
FIELD_W = 800, FIELD_H = 600, PADDLE_H = 80
\
def move_ball(ball, vel): return (ball[0]+vel[0], ball[1]+vel[1])  # moves by velocity
\
def bounce(vel, axis): v=[vel[0],vel[1]]; v[axis]=-v[axis]; return (v[0],v[1])  # axis 0=x,1=y
\
def move_paddle(paddle, dy): return max(0, min(FIELD_H-PADDLE_H, paddle+dy))  # clamps to [0, FIELD_H-PADDLE_H]
\
def point_scored(ball): return ball[0] < 0 or ball[0] > FIELD_W  # True when out of horizontal bounds
\
Also add a minimal text-only runner pong.py that uses those functions, and test_pong.py that imports pong_logic and checks the same behaviors with plain python asserts. Run python3 test_pong.py and confirm it passes.".into(),
            4,
            vec![],
        ),
        dsflash_scenario_spec(
            "flappy",
            "Create a tiny Flappy Bird clone in Python with a STABLE headless API contract. Put the pure bird physics in flappy_logic.py with no GUI dependency and NO pygame import. flappy_logic.py MUST define exactly these functions and top-level constants that an external checker imports and calls:
\
GRAV = 1.0, FLAP_VY = -8.0, BIRD_R = 8.0, PIPE_W = 60.0
\
def update_bird(b): x,y,vy=b; return (x, y+vy, vy+GRAV)  # gravity updates velocity and position
\
def flap(b): x,y,vy=b; return (x, y, FLAP_VY)  # resets vertical velocity to FLAP_VY
\
def collides(bird, pipes): x,y,vy=bird; return any(abs(px-x) < PIPE_W/2+BIRD_R and ((y-BIRD_R) < top or (y+BIRD_R) > bottom) for (px,top,bottom) in pipes)
\
def passed(bird, pipe): return bird[0] > pipe[0] + PIPE_W/2
\
def score(bird, pipes): return sum(1 for p in pipes if passed(bird, p))
\
Also add an ASCII-only runner flappy.py that uses these functions and test_flappy.py that imports flappy_logic and checks gravity, flapping, a collision and scoring with plain python asserts. Run python3 test_flappy.py and confirm it passes.".into(),
            4,
            vec![],
        ),
        dsflash_scenario_spec(
            "encryption",
            "Create a small Rust LIBRARY crate whose package name is exactly filecrypt (the [package] name = filecrypt) for file encryption. Use an established crypto crate from crates.io as a real [dependencies] entry (aes-gcm, chacha20poly1305, or aead), and derive the key from the password. src/lib.rs MUST expose EXACTLY this public API that an external consumer compiles against:
\
pub fn encrypt(password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String>;
\
pub fn decrypt(password: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String>;
\
A hidden consumer depends on this crate by path and runs tests that assert: ciphertext differs from the plaintext, encrypt-then-decrypt roundtrips, decrypt with a WRONG password returns Err, and decrypt of a TAMPERED ciphertext returns Err. Wire the crate's own roundtrip and wrong-password tests into cargo test and run them; they must pass. cargo is available on this machine and may fetch crates from crates.io.".into(),
            6,
            vec![],
        ),
    ]
}

/// Build a [`BbResult`] for tests, defaulting the raw streams empty.
pub fn test_result(ok: bool) -> BbResult {
    BbResult {
        ok,
        status: if ok { "ok".into() } else { "error".into() },
        exit: if ok { Some(0) } else { Some(3) },
        timed_out: false,
        session_id: if ok { "sess".into() } else { String::new() },
        turn: 1,
        attempt: 1,
        branch_id: "b1".into(),
        branch: false,
        replayed: false,
        tool_calls: if ok { 2 } else { 0 },
        prompt_digest: String::new(),
        summary: String::new(),
        error_code: String::new(),
        error_message: String::new(),
        raw_stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        combined_truncated: false,
    }
}

#[cfg(test)]
mod tests;
