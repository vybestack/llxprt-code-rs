//! Runner adapters and deterministic fixture expansion for context evals (#37).
//!
//! The Rust adapter drives the compiled `llxprt-code-rs` binary with an isolated
//! `LLXPRT_CONFIG_HOME`, a temporary workspace, and a generated loopback profile. The
//! TypeScript adapter drives the sibling implementation through its Bun CLI with the same
//! manifest, isolated settings, and the same loopback; it validates that a scenario
//! exercises a real context wall and is never an oracle.

use crate::context_eval::manifest::Scenario;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Everything an adapter needs that is not runner argv.
pub struct Prepared {
    pub config_home: PathBuf,
    pub workspace: PathBuf,
    pub profile_name: String,
    pub bulk: Vec<PathBuf>,
    pub fixture_digests: Vec<String>,
    pub session: String,
}

/// Deterministically expand a fixture into one bounded bulk file per scripted round.
///
/// Every round's file embeds unique `ctxeval-<round>-<block>` index lines, so no two
/// admitted tool results are byte-identical and deduplication cannot collapse the wall.
pub fn expand_fixture(
    fixtures: &Path,
    fixture: &str,
    rounds: u32,
    block_bytes: usize,
    out_dir: &Path,
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let seed =
        fs::read(fixtures.join(fixture)).map_err(|e| format!("read fixture {fixture}: {e}"))?;
    let seed_text = String::from_utf8_lossy(&seed).to_string();
    if !out_dir.is_absolute() {
        return Err(format!(
            "harness path bug: bulk dir {} is not absolute",
            out_dir.display()
        ));
    }
    fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let mut files = Vec::new();
    let mut digests = Vec::new();
    for round in 0..rounds {
        let mut body = String::new();
        let mut block = 0usize;
        while body.len() < block_bytes {
            body.push_str(&format!("ctxeval-{round}-{block} "));
            body.push_str(&seed_text);
            body.push('\n');
            block += 1;
        }
        body.truncate(block_bytes.max(1));
        let path = out_dir.join(format!("round-{round:02}.txt"));
        crate::harness::publish_create_only_file(&path, body.as_bytes())
            .map_err(|e| format!("expand fixture {}: {e:?}", path.display()))?;
        files.push(path);
        digests.push(hex(&Sha256::digest(body.as_bytes())));
    }
    Ok((files, digests))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Digest of a file's bytes.
pub fn file_digest(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(hex(&Sha256::digest(&bytes)))
}

/// Materialise the isolated config home, generated profile, workspace, and bulk fixtures.
pub fn prepare(
    root: &Path,
    scen: &Scenario,
    base_url: &str,
    bulk: Vec<PathBuf>,
    fixture_digests: Vec<String>,
) -> Result<Prepared, String> {
    if !root.is_absolute() {
        return Err(format!(
            "harness path bug: prepared root {} is not absolute",
            root.display()
        ));
    }
    let run = root.join(format!("run-{}", crate::harness::uniq()));
    let config_home = run.join("config");
    let workspace = run.join("ws");
    for dir in [&config_home, &workspace] {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    write_profile(&config_home, scen, base_url)?;
    Ok(Prepared {
        config_home,
        workspace,
        profile_name: scen.profile.name.clone(),
        bulk,
        fixture_digests,
        session: format!("ctxeval-{}", crate::harness::uniq()),
    })
}

fn write_profile(config_home: &Path, scen: &Scenario, base_url: &str) -> Result<(), String> {
    let dir = config_home.join("profiles");
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // The loopback never validates credentials; the inline value is a synthetic marker so
    // the CLI never touches a native credential store or a real provider. Only profile
    // keys this CLI accepts for a plain loopback Chat provider are emitted; the
    // ordinary sibling settings are inert here, so only the applied ones are sent.
    // `stream-idle-timeout-ms` is dsflash-only and would be rejected as model-config.
    // The effective context limit comes from the scenario's arm-specific runtime config
    // (GAP-H7): arm selection must change installed runtime behavior, not just a label.
    let profile = serde_json::json!({
        "version": 1,
        "provider": scen.profile.provider,
        "model": scen.profile.model,
        "modelParams": { "temperature": 0.0 },
        "ephemeralSettings": {
            "auth-key": "ctxeval-loopback-local-stub",
            "base-url": base_url,
            "context-limit": scen.runtime.context_limit,
            "maxOutputTokens": scen.profile.max_output_tokens,
        },
    });
    let name = &scen.profile.name;
    let path = dir.join(format!("{name}.json"));
    crate::harness::publish_create_only_file(&path, profile.to_string().as_bytes())
        .map_err(|e| format!("write profile {}: {e:?}", path.display()))?;
    Ok(())
}

/// Build the Rust adapter argv for one turn of a scenario.
pub fn rust_args(
    session: &str,
    workspace: &Path,
    prompt: &str,
    turn: Option<u32>,
    profile: &str,
) -> Vec<String> {
    let mut args = vec![
        "--session".into(),
        session.to_string(),
        "--cwd".into(),
        workspace.display().to_string(),
        "-p".into(),
        prompt.to_string(),
        "--profile".into(),
        profile.to_string(),
        "--allow-insecure-http".into(),
    ];
    if let Some(t) = turn {
        args.push("--turn".into());
        args.push(t.to_string());
    }
    args
}

/// Build the TypeScript reference adapter argv for one turn of a scenario.
///
/// The sibling implementation is started through its Bun CLI with `--prompt` and JSON
/// output. Its CLI has no `--session` flag: the loopback endpoint, model, and a synthetic
/// key arrive as flags (never a real credential), approvals are pre-granted so a
/// non-interactive run cannot stall on a prompt, and isolated settings arrive through the
/// environment the adapter sets. This adapter validates that a scenario exercises a real
/// context wall; it is never an oracle.
pub fn ts_args(prompt: &str, base_url: &str, model: &str) -> Vec<String> {
    vec![
        "--preload".into(),
        "./scripts/dev-env.ts".into(),
        "packages/cli/index.ts".into(),
        "--prompt".into(),
        prompt.to_string(),
        "--output-format".into(),
        "json".into(),
        "--quiet".into(),
        "--approval-mode".into(),
        "yolo".into(),
        "--baseurl".into(),
        base_url.to_string(),
        "--provider".into(),
        "openai".into(),
        "--key".into(),
        "ctxeval-loopback-local-stub".into(),
        "--model".into(),
        model.to_string(),
    ]
}

/// Default sibling repository root for the TypeScript reference runner.
pub const TS_ROOT_DEFAULT: &str = "/Users/acoliver/projects/llxprt/agent/main/llxprt-code";
