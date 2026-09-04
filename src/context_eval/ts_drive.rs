//! The TypeScript reference drive, split from the parent module only to respect the
//! per-file effective-LOC ceiling; the drive and its TS-only helpers move together as
//! one coherent group and keep the same semantics.
//!
//! Calibration only: the reference runner's verdict is reported and never gates a
//! phase, because the sibling implementation is not the oracle.

use super::{
    absolute_child, bounded, publish, ts_context_limit_hit, Drive, Options, TURN_TIMEOUT_SECS,
};
use crate::context_eval::grader::{self, Evidence};
use crate::context_eval::loopback::Loopback;
use crate::context_eval::manifest::Scenario;
use crate::context_eval::runner;
use crate::context_eval::secrets;
use crate::harness;
use crate::process::{self, CmdSpec};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Drive the TypeScript reference runner. Calibration only: its verdict is reported and
/// never gates the phase, because the sibling implementation is not the oracle.
pub(super) fn drive_typescript(scen: &Scenario, opts: &Options) -> Result<Drive, String> {
    let out_dir = opts
        .out_root
        .join(format!("{}-{}", scen.id, harness::uniq()));
    let (bulk, digests) = runner::expand_fixture(
        &opts.fixtures_dir(),
        &scen.wall.fixture,
        scen.wall.tool_rounds,
        scen.wall.tool_output_bytes,
        &out_dir.join("bulk"),
    )?;
    let marker = scen
        .assertions
        .required_final_marker
        .clone()
        .unwrap_or_else(|| "CTXEVAL-FINAL".to_string());
    // `Loopback::start` only binds the port; the scripted rounds arrive through
    // `set_bulk` once the fixtures exist, exactly as the Rust drive does. Without the
    // hand-off the stub serves an empty script and answers every turn with the final
    // marker, so no wall is ever exercised.
    let rounds = bulk.len();
    let server = Loopback::start(rounds, Vec::new(), &marker, scen.wall.tool_output_bytes);
    server.set_bulk(bulk);
    let url = server.base_url();
    let settings = absolute_child(&out_dir, "settings")?;
    fs::create_dir_all(&settings).map_err(|e| format!("create settings: {e}"))?;
    let mut evidence = Evidence {
        turns_total: 1,
        ..Evidence::default()
    };
    // The TS reference runner has no fault or recovery machinery; the harness records
    // that honestly rather than simulating an execution.
    // The reference runner executes no faults, so it must not report any: an empty list
    // here is the honest observation (the grader then fails the recovery dimension for a
    // scenario that declares faults), and it replaces the no-op assignment that used to
    // stand in for a real "did the TS drive execute anything" check.
    evidence.faults_executed.clear();
    let args = runner::ts_args(&scen.stimulus.prompt, &url, &scen.profile.model);
    let outcome = process::run_cmd(CmdSpec {
        program: opts.ts_bin.clone(),
        args,
        cwd: Some(opts.ts_root.clone()),
        cwd_fd: None,
        env_add: vec![
            ("LLXPRT_CONFIG_HOME".into(), settings.display().to_string()),
            ("XDG_CONFIG_HOME".into(), settings.display().to_string()),
            ("CTXEVAL_LOOPBACK_BASE_URL".into(), url),
        ],
        timeout: Duration::from_secs(TURN_TIMEOUT_SECS),
        max_output: 32 * 1024 * 1024,
    });
    let obs = server.snapshot();
    server.stop();
    evidence.provider_requests = obs.requests.len();
    evidence.tool_calls_scripted = obs.tool_calls_issued;
    evidence.final_response_issued = obs.final_response_issued;
    let text = match &outcome {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(_) => String::new(),
    };
    // The TS drive's leakage scan is real, exactly like the Rust drive's: the isolated
    // settings tree, the harness artifacts, and the captured streams are all scanned, so
    // the reference runner cannot publish a clean it never earned. Before the fix the TS
    // drive published `"clean": true` without scanning anything at all.
    scan_ts_outputs(&mut evidence, &out_dir, &settings, &text);
    match outcome {
        Ok(o) => {
            if o.timed_out || o.status.is_none() {
                evidence.harness_error = true;
            }
            if o.status == Some(0) && serde_json::from_slice::<Value>(&o.stdout).is_ok() {
                evidence.turns_ok = 1;
                evidence.last_ok_stdout = o.stdout.clone();
                evidence.last_ok_summary = String::from_utf8_lossy(&o.stdout).to_string();
            }
            save_ts_artifacts(&out_dir, &o.stdout, &o.stderr)?;
        }
        Err(e) => {
            evidence.harness_error = true;
            harness::eprint_status(&format!("typescript reference spawn failed: {e}"));
        }
    }
    // Typed classification where the reference runner offers one (#7): the TS runner has
    // no typed error contract, so the residual substring fallback below is the only
    // classification available for it. Its failure modes are documented at the site:
    // a *false positive* whenever the run's prose merely mentions "context" or
    // "compress" without hitting a wall, and a *false negative* whenever a wall is hit
    // but reported with other wording. That is why the TS drive is calibration-only and
    // never gates a phase: its classification cannot be trusted as evidence.
    evidence.context_limit_hit = ts_context_limit_hit(&text);
    let graded = grader::grade(scen, &evidence);
    let verdict = grader::verdict(scen, &graded);
    Ok((evidence, graded, verdict, digests, true))
}

fn save_ts_artifacts(out_dir: &Path, stdout: &[u8], stderr: &[u8]) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    publish(&out_dir.join("ts.stdout"), &bounded(stdout))?;
    publish(&out_dir.join("ts.stderr"), &bounded(stderr))?;
    Ok(())
}

/// Leakage scan over the TypeScript reference drive's own outputs: the isolated settings
/// tree, the harness artifacts, and the captured streams. The bulk fixtures are the
/// plant's input and are excluded, exactly as in the Rust drive's scan.
fn scan_ts_outputs(evidence: &mut Evidence, out_dir: &Path, settings: &Path, captured: &str) {
    let bulk_dir = out_dir.join("bulk");
    evidence.leaks = secrets::scan_tree_skipping(settings, Some(bulk_dir.as_path()))
        .into_iter()
        .map(|(marker, found)| (marker, format!("settings: {found}")))
        .chain(
            secrets::scan_tree_skipping(out_dir, Some(bulk_dir.as_path()))
                .into_iter()
                .map(|(marker, found)| (marker, format!("harness artifacts: {found}"))),
        )
        .collect();
    for marker in secrets::scan_bytes(captured.as_bytes()) {
        evidence
            .leaks
            .push((marker.to_string(), "captured stream".to_string()));
    }
}
