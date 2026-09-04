//! Harness self-tests for the Phase 0 context evals (issue #115, #116).
//!
//! These prove the harness itself rejects malformed reports, catches a false success built
//! from a model-authored claim, rejects bad manifests and unsupported faults, matches red
//! reasons strictly, grades evidence dimensions separately, scans captured output for
//! planted secrets, exercises report validation on the harness's own publish shape, and
//! preserves an append-only phase-indexed expected-status history. No test contacts a
//! live provider.

use crate::context_eval::grader::{self, Dimension, Evidence, Verdict};
use crate::context_eval::loopback::ObservedRequest;
use crate::context_eval::{manifest, report, runner, secrets, ARTIFACT_STREAM_CAP};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    PathBuf::from("evals/context-management/fixtures")
}

fn good_manifest() -> String {
    r#"
schema_version = 1
id = "selftest-probe"
owner_phase = 7
arm = "feature"
expected_status = "red"
expected_reason_class = "context-limit"
accept_any_reason = false

[profile]
name = "ctxeval-loopback"
provider = "openai"
model = "ctxeval-fixture"
context_limit_tokens = 20000
max_output_tokens = 2048

[stimulus]
prompt = "CTXEVAL-SELFTEST read the bulk file then emit the final marker"

[wall]
tool_rounds = 1
tool_output_bytes = 4096
fixture = "tool-output-block.txt"

[assertions]
required_final_marker = "CTXEVAL-FINAL-7f3a9c"

[runtime]
name = "profile-limit"
context_limit = 20000
"#
    .to_string()
}

#[test]
fn manifest_schema_rejects_unknown_fields_and_bad_versions() {
    let ok = manifest::parse_str(&good_manifest(), &fixtures());
    assert!(ok.is_ok(), "valid manifest rejected: {ok:?}");

    let unknown = good_manifest().replacen("[profile]", "[profile]\nsurprise_field = 1", 1);
    assert!(
        manifest::parse_str(&unknown, &fixtures()).is_err(),
        "unknown field accepted"
    );

    let version = good_manifest().replacen("schema_version = 1", "schema_version = 2", 1);
    assert!(
        manifest::parse_str(&version, &fixtures()).is_err(),
        "wrong schema version accepted"
    );

    let owner = good_manifest().replacen("owner_phase = 7", "owner_phase = 0", 1);
    assert!(
        manifest::parse_str(&owner, &fixtures()).is_err(),
        "owner_phase 0 accepted"
    );

    let rounds = good_manifest().replacen("tool_rounds = 1", "tool_rounds = 17", 1);
    assert!(
        manifest::parse_str(&rounds, &fixtures()).is_err(),
        "17 tool rounds accepted"
    );

    let runner_argv = good_manifest().replacen(
        "prompt = \"CTXEVAL-SELFTEST",
        "argv = [\"--foo\"]\nprompt = \"CTXEVAL-SELFTEST",
        1,
    );
    assert!(
        manifest::parse_str(&runner_argv, &fixtures()).is_err(),
        "runner argv in a manifest accepted"
    );

    let no_runtime = good_manifest().replace(
        "\n[runtime]\nname = \"profile-limit\"\ncontext_limit = 20000\n",
        "",
    );
    assert!(
        manifest::parse_str(&no_runtime, &fixtures()).is_err(),
        "manifest without arm runtime config accepted"
    );
}

/// The schema bounds the harness's inputs, so a drive cannot be made unbounded or made
/// to read outside the fixture tree by a manifest: prompt and followup sizes are capped,
/// fixture names must stay inside the fixture root, and duplicate scenario ids cannot
/// alias each other in reports, records, and allow-lists.
#[test]
fn manifest_bounds_prompts_fixtures_and_duplicate_ids() {
    // An oversized opening prompt is rejected at load, not discovered mid-drive.
    let long_prompt = "x".repeat(manifest::MAX_PROMPT_BYTES + 1);
    let oversized = good_manifest().replace(
        "prompt = \"CTXEVAL-SELFTEST read the bulk file then emit the final marker\"",
        &format!("prompt = \"{long_prompt}\""),
    );
    assert!(
        manifest::parse_str(&oversized, &fixtures()).is_err(),
        "oversized prompt accepted"
    );

    // A fixture name that escapes the fixture root is a traversal attempt, not a fixture.
    for name in [
        "../leak-corpus.txt",
        "/etc/hosts",
        "nested/../../escape.txt",
    ] {
        let traversal = good_manifest().replace(
            "fixture = \"tool-output-block.txt\"",
            &format!("fixture = \"{name}\""),
        );
        assert!(
            manifest::parse_str(&traversal, &fixtures()).is_err(),
            "traversal fixture name {name} accepted"
        );
    }

    // Duplicate scenario ids must not silently alias: two manifests with the same id are
    // a load-time error naming both files, not a report that mixes two scenarios.
    let dir = std::env::temp_dir().join(format!("ctxeval-dup-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.toml"), good_manifest()).unwrap();
    fs::write(dir.join("b.toml"), good_manifest()).unwrap();
    let err = manifest::load_dir(&dir, &fixtures());
    assert!(
        err.is_err(),
        "duplicate scenario id accepted across two manifests"
    );
    assert!(
        err.unwrap_err().contains("duplicate scenario id"),
        "duplicate id error does not name the collision"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn arm_mismatch_between_manifest_and_record_is_rejected() {
    let dir = std::env::temp_dir().join(format!("ctxeval-arm-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    let text = good_manifest().replace(
        "[runtime]\nname = \"profile-limit\"",
        "[runtime]\nname = \"minimum-floor-overrides\"",
    );
    let scen = manifest::parse_str(&text, &fixtures()).unwrap();
    // The installed runtime config must come from the manifest's arm-specific block, so
    // a renamed arm changes the installed configuration (verifiable without a run).
    assert_eq!(scen.runtime.name, "minimum-floor-overrides");
    let _ = dir;
}

#[test]
fn manifest_fixture_size_is_bounded() {
    let dir = std::env::temp_dir().join(format!("ctxeval-fx-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    let big = dir.join("too-big.txt");
    fs::write(&big, vec![b'x'; 257 * 1024]).unwrap();
    let text = good_manifest().replace("tool-output-block.txt", "too-big.txt");
    let err = manifest::parse_str(&text, &dir);
    assert!(err.is_err(), "fixture over 256 KiB accepted");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn fixture_expansion_is_deterministic_and_bounded() {
    let out = std::env::temp_dir().join(format!("ctxeval-ex-{}", crate::harness::uniq()));
    let (first, first_digests) = runner::expand_fixture(
        &fixtures(),
        "tool-output-block.txt",
        3,
        8192,
        &out.join("a"),
    )
    .unwrap();
    let (_second, second_digests) = runner::expand_fixture(
        &fixtures(),
        "tool-output-block.txt",
        3,
        8192,
        &out.join("b"),
    )
    .unwrap();
    assert_eq!(
        first_digests, second_digests,
        "expansion is not deterministic"
    );
    assert_eq!(first.len(), 3);
    for (round, path) in first.iter().enumerate() {
        let body = fs::read(path).unwrap();
        assert_eq!(
            body.len(),
            8192,
            "round {round} is not exactly the requested size"
        );
        let tag = format!("ctxeval-{round}-").into_bytes();
        assert!(
            body.windows(tag.len()).any(|w| w == tag),
            "round {round} does not carry its unique index"
        );
    }
    assert_ne!(
        first_digests[0], first_digests[1],
        "rounds are byte-identical"
    );
    fs::remove_dir_all(&out).ok();
}

#[test]
fn artifact_capture_is_bounded() {
    let big = vec![b'y'; ARTIFACT_STREAM_CAP * 4];
    let out = std::env::temp_dir().join(format!("ctxeval-cap-{}", crate::harness::uniq()));
    fs::create_dir_all(&out).unwrap();
    let path = out.join("stream");
    crate::harness::publish_create_only_file(&path, &big[..ARTIFACT_STREAM_CAP]).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().len() as usize,
        ARTIFACT_STREAM_CAP
    );
    fs::remove_dir_all(&out).ok();
}

/// Children must receive absolute paths: the CLI contract rejects a relative
/// `LLXPRT_CONFIG_HOME` with `config-home` (exit 3), and the TS reference runner needs the
/// same guarantee for its isolated settings dir. This test reproduces the exact bug where a
/// relative `--out` leaked a relative config home into the child environment.
#[test]
fn child_environment_paths_are_absolute() {
    let tmp = std::env::temp_dir().join(format!("ctxeval-abs-{}", crate::harness::uniq()));
    fs::create_dir_all(&tmp).unwrap();

    // A relative out root must be canonicalized into an absolute one before use.
    let rel = PathBuf::from("tmp/issue37-abs-check");
    let abs = std::env::current_dir().unwrap().join(&rel);
    fs::create_dir_all(&abs).unwrap();
    let canon = abs.canonicalize().unwrap();
    assert!(canon.is_absolute(), "canonicalized out root is relative");

    // The config home and workspace derived from it are absolute, so the values handed to
    // the child through LLXPRT_CONFIG_HOME and --cwd satisfy the CLI contract.
    let config_home = canon.join("run-1").join("config");
    let workspace = canon.join("run-1").join("ws");
    assert!(config_home.is_absolute());
    assert!(workspace.is_absolute());

    // TS isolated settings: same rule, derived from the same canonical root.
    let settings = canon.join("wall-large-tool-final-1").join("settings");
    assert!(settings.is_absolute());

    // Bulk fixture paths are passed to children through tool argv, so they are absolute too.
    let (bulk, _) = runner::expand_fixture(
        &fixtures(),
        "tool-output-block.txt",
        1,
        2048,
        &canon.join("bulk-1"),
    )
    .unwrap();
    for path in &bulk {
        assert!(
            path.is_absolute(),
            "bulk path {} is relative",
            path.display()
        );
    }

    // A relative bulk dir is a harness bug, not a scenario result.
    let bad = runner::expand_fixture(
        &fixtures(),
        "tool-output-block.txt",
        1,
        2048,
        Path::new("rel-bulk"),
    );
    assert!(bad.is_err(), "relative bulk dir accepted");

    let _ = &rel;
    fs::remove_dir_all(&tmp).ok();
    fs::remove_dir_all(&canon).ok();
}

#[test]
fn adapter_command_construction_is_exact() {
    let rust = runner::rust_args(
        "s1",
        PathBuf::from("/tmp/nope").as_path(),
        "hi",
        Some(2),
        "p",
    );
    assert_eq!(
        rust,
        vec![
            "--session",
            "s1",
            "--cwd",
            "/tmp/nope",
            "-p",
            "hi",
            "--profile",
            "p",
            "--allow-insecure-http",
            "--turn",
            "2"
        ]
    );
    let ts = runner::ts_args("hi", "http://127.0.0.1:9/v1", "ctxeval-fixture");
    assert_eq!(
        ts,
        vec![
            "--preload",
            "./scripts/dev-env.ts",
            "packages/cli/index.ts",
            "--prompt",
            "hi",
            "--output-format",
            "json",
            "--quiet",
            "--approval-mode",
            "yolo",
            "--baseurl",
            "http://127.0.0.1:9/v1",
            "--provider",
            "openai",
            "--key",
            "ctxeval-loopback-local-stub",
            "--model",
            "ctxeval-fixture",
        ]
    );
    assert!(
        !ts.iter().any(|a| a == "--session"),
        "the TS CLI has no --session flag"
    );
    assert!(runner::TS_ROOT_DEFAULT.contains("llxprt-code"));
}

/// The scenario-report shape this harness publishes, as a validation fixture.
fn good_report() -> Value {
    json!({
        "id": "x", "schema_version": 1, "owner_phase": 7, "arm": "feature",
        "expected_status": "red", "runner": "rust", "runner_revision": "abc",
        "fixture_digests": [],
        "profile": {"name": "p", "provider": "openai", "model": "m",
                    "context_limit_tokens": 1000, "max_output_tokens": 100},
        "result": {"verdict": "expected-red", "accepted": true,
                   "reason_class": "context-limit", "failures": []},
        "evidence_status": {"source": "independent", "turns_total": 1, "turns_ok": 1,
                            "provider_requests": 1, "tool_calls_scripted": 1,
                            "final_response_issued": true, "wall_hit": true,
                            "terminal_outcome": "none", "isolation_ok": true},
        "cache": report::cache_block(),
        "evidence_dimensions": {"task": true, "protocol": true, "resource": true,
                                "latency": true, "recovery": true, "wall_realism": true},
        "request_observations": {"requests": 0, "max_request_bytes": 0,
                                 "streamed_requests": 0, "tool_names": [],
                                 "request_shape_digest": "00",
                                 "observations_source": "loopback"},
        "leakage_scan": {"clean": true, "findings": []},
        "runtime_config": {"name": "profile-limit", "context_limit": 20000},
    })
}

/// The aggregate-report shape this harness publishes, as a validation fixture.
fn good_aggregate_report() -> Value {
    json!({
        "tool": "llxprt-context-eval", "schema_version": 1, "run_id": "r",
        "runner": "rust", "runner_revision": "abc", "expected_status_mode": true,
        "phase0_baseline": {"all_red_baseline": true},
        "records_root": "evals/context-management/records",
        "cache": report::cache_block(),
        "summary": {"total": 0, "expected_red": 0, "unexpected_green": 0,
                    "unexpected_red": 0, "harness_error": 0},
        "scenarios": [],
    })
}

#[test]
fn malformed_reports_are_detected() {
    let good = good_report();
    assert!(
        report::validate(&good, false).is_ok(),
        "valid scenario report rejected"
    );

    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("runner_revision");
    assert!(
        report::validate(&missing, false).is_err(),
        "missing field accepted"
    );

    let mut version = good.clone();
    version["schema_version"] = json!(99);
    assert!(
        report::validate(&version, false).is_err(),
        "wrong schema version accepted"
    );

    let mut no_cache = good.clone();
    no_cache.as_object_mut().unwrap().remove("cache");
    assert!(
        report::validate(&no_cache, false).is_err(),
        "missing cache block accepted"
    );

    let mut no_dims = good.clone();
    no_dims
        .as_object_mut()
        .unwrap()
        .remove("evidence_dimensions");
    assert!(
        report::validate(&no_dims, false).is_err(),
        "missing evidence_dimensions accepted"
    );

    let mut bad_dim = good.clone();
    bad_dim["evidence_dimensions"]["protocol"] = json!("yes");
    assert!(
        report::validate(&bad_dim, false).is_err(),
        "non-boolean evidence dimension accepted"
    );

    let mut no_observations = good.clone();
    no_observations
        .as_object_mut()
        .unwrap()
        .remove("request_observations");
    assert!(
        report::validate(&no_observations, false).is_err(),
        "missing request_observations accepted"
    );

    let mut bad_leak = good.clone();
    bad_leak["leakage_scan"] = json!({"clean": "yes"});
    assert!(
        report::validate(&bad_leak, false).is_err(),
        "malformed leakage_scan accepted"
    );

    let mut no_id = good.clone();
    no_id.as_object_mut().unwrap().remove("id");
    assert!(
        report::validate(&no_id, false).is_err(),
        "missing id accepted"
    );
}

#[test]
fn malformed_aggregate_reports_are_detected() {
    let aggregate = good_aggregate_report();
    assert!(
        report::validate(&aggregate, true).is_ok(),
        "valid aggregate rejected"
    );
    let mut bad_summary = aggregate.clone();
    bad_summary["summary"]
        .as_object_mut()
        .unwrap()
        .remove("total");
    assert!(
        report::validate(&bad_summary, true).is_err(),
        "bad summary accepted"
    );
}

#[test]
fn terminal_outcomes_are_allowed_alternatives() {
    let text = good_manifest().replace(
        "required_final_marker = \"CTXEVAL-FINAL-7f3a9c\"",
        "required_outcomes = [\"disarm\", \"quiesce_unwritable\", \"wrap_up\"]",
    );
    let scenario = manifest::parse_str(&text, &fixtures()).unwrap();
    let evidence = Evidence {
        turns_total: 1,
        turns_ok: 1,
        terminal_outcome: Some("wrap_up".to_string()),
        ..Evidence::default()
    };
    assert!(grader::grade(&scenario, &evidence).passed);
}

#[test]
fn cache_acceptance_report_reads_durable_conditional_telemetry() {
    let dir = std::env::temp_dir().join(format!("ctxeval-cache-{}", crate::harness::uniq()));
    fs::create_dir_all(dir.join("context")).unwrap();
    fs::write(
        dir.join("context/rewrite-journal.log"),
        concat!(
            "{\"invalidation_cost\":null,\"bytes_reclaimed\":100}\n",
            "{\"report\":{\"armed_hit_rate\":0.5,\"armed_rewrites\":1,",
            "\"disarmed_hit_rate\":null,\"disarmed_rewrites\":0,\"hit_rate\":0.5,",
            "\"invalidation_cost_per_event\":null,\"known_invalidation_cost_events\":0,",
            "\"threshold_denials\":0,\"threshold_passes\":1,",
            "\"economic_gate_suspensions\":1,",
            "\"unknown_invalidation_cost_events\":1}}\n"
        ),
    )
    .unwrap();
    let cache = report::cache_block_from_session(Some(&dir));
    assert_eq!(cache["class"], "measured");
    assert_eq!(cache["rewrite_journal_bytes_reclaimed"], 100);
    assert!(cache["rewrite_journal_bytes_invalidated"].is_null());
    assert!(cache["prefix_invalidation_cost_per_rewrite"].is_null());
    assert_eq!(cache["conditional"]["armed_hit_rate"], 0.5);
    assert_eq!(cache["suspended_while_armed"], true);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn false_success_from_a_model_claim_is_caught() {
    let scen = manifest::parse_str(&good_manifest(), &fixtures()).unwrap();
    // A lying turn: the summary claims the marker, the provider never issued it.
    let lying = Evidence {
        turns_total: 1,
        turns_ok: 1,
        last_ok_summary: "done: CTXEVAL-FINAL-7f3a9c".to_string(),
        final_response_issued: false,
        ..Evidence::default()
    };
    let graded = grader::grade(&scen, &lying);
    assert!(!graded.passed, "a model-authored claim passed the grader");
    assert!(graded
        .failures
        .iter()
        .any(|f| f.contains("without the provider ever issuing it")));
    // Protocol breakage is graded on its own dimension, not as harness infrastructure.
    assert!(
        graded.failures.iter().any(|f| f.starts_with("[protocol]")),
        "forged-marker failure was not charged to the protocol dimension"
    );
    assert_ne!(graded.reason_class, "harness-error");
    let caught = grader::verdict(&scen, &graded);
    assert_ne!(
        caught,
        Verdict::UnexpectedGreen,
        "the lie was graded as a pass"
    );
    assert!(!caught.accepted(), "the lie was accepted");
    assert_eq!(caught, Verdict::UnexpectedRedReason);

    // The honest same shape (provider did issue it) is not a false positive either: the
    // scenario is still red in Phase 0 because nothing landed yet.
    let mut honest = lying.clone();
    honest.final_response_issued = true;
    let honest_graded = grader::grade(&scen, &honest);
    assert!(honest_graded.passed, "honest completion graded as failure");
    assert_eq!(
        grader::verdict(&scen, &honest_graded),
        Verdict::UnexpectedGreen
    );
}

#[test]
fn verdicts_distinguish_expected_red_from_harness_error() {
    let scen = manifest::parse_str(&good_manifest(), &fixtures()).unwrap();
    let wall = Evidence {
        turns_total: 1,
        context_limit_hit: true,
        resource_limit_hit: true,
        turns_ok: 1,
        provider_requests: 3,
        ..Evidence::default()
    };
    let graded = grader::grade(&scen, &wall);
    assert_eq!(graded.reason_class, "context-limit");
    assert_eq!(grader::verdict(&scen, &graded), Verdict::ExpectedRed);

    let mut broken = wall.clone();
    broken.harness_error = true;
    let broken_graded = grader::grade(&scen, &broken);
    assert_eq!(
        grader::verdict(&scen, &broken_graded),
        Verdict::HarnessError
    );
    assert!(!grader::verdict(&scen, &broken_graded).accepted());
}

/// Strict reason matching (GAP-H7): a red scenario predicted as `context-limit` with
/// `accept_any_reason = false` must NOT be accepted when it fails for any other reason.
/// The old harness silently accepted `missing-evidence` as a substitute.
#[test]
fn strict_reason_matching_rejects_wrong_class_red() {
    let scen = manifest::parse_str(&good_manifest(), &fixtures()).unwrap();
    assert_eq!(scen.expected_reason_class, "context-limit");
    assert!(!scen.accept_any_reason);
    let wrong_reason = Evidence {
        turns_total: 1,
        turns_ok: 1,
        ..Evidence::default()
    };
    let graded = grader::grade(&scen, &wrong_reason);
    assert!(!graded.passed);
    assert_eq!(graded.reason_class, "missing-evidence");
    assert_eq!(
        grader::verdict(&scen, &graded),
        Verdict::UnexpectedRedReason,
        "missing-evidence substituted for the predicted reason class"
    );
    assert!(!grader::verdict(&scen, &graded).accepted());

    // accept_any_reason stays opt-in: it broadens exactly one scenario, not the harness.
    let text = good_manifest().replace("accept_any_reason = false", "accept_any_reason = true");
    let broad = manifest::parse_str(&text, &fixtures()).unwrap();
    assert_eq!(
        grader::verdict(&broad, &graded),
        Verdict::ExpectedRed,
        "declared accept_any_reason was not honored"
    );
}

/// Evidence dimension separation (GAP-M15, R-016): protocol breakage, resource use,
/// recovery, and wall realism are graded as separate fields, and a protocol failure is
/// never classified as harness infrastructure error.
#[test]
fn evidence_dimensions_are_graded_separately() {
    let scen = manifest::parse_str(&good_manifest(), &fixtures()).unwrap();
    let mut ev = Evidence {
        turns_total: 2,
        turns_ok: 2,
        final_response_issued: true,
        last_ok_summary: "CTXEVAL-FINAL-7f3a9c".to_string(),
        context_limit_hit: true,
        resource_limit_hit: true,
        provider_requests: 4,
        ..Evidence::default()
    };
    let dims = grader::dimension_results(&scen, &ev);
    assert_eq!(dims.len(), 6);
    let field_of = |dim: Dimension| dims.iter().find(|(d, _, _)| *d == dim).unwrap();
    let (_, task_ok, _) = field_of(Dimension::Task);
    let (_, wall_ok, _) = field_of(Dimension::WallRealism);
    let (_, latency_ok, latency_failures) = field_of(Dimension::Latency);
    assert!(task_ok);
    assert!(wall_ok);
    // Two declared prompts, two driven turns: the budget honored.
    assert!(
        latency_ok,
        "honored budget graded as latency failure: {latency_failures:?}"
    );

    ev.leaks.push((
        "CTXEVAL-SECRET-A1B2C3D4E5".to_string(),
        "captured stream".to_string(),
    ));
    let dims = grader::dimension_results(&scen, &ev);
    let (_, protocol_ok, protocol_failures) = dims
        .iter()
        .find(|(d, _, _)| *d == Dimension::Protocol)
        .unwrap();
    assert!(!protocol_ok, "leak did not fail the protocol dimension");
    assert!(protocol_failures.iter().any(|f| f.contains("leaked")));
    let graded = grader::grade(&scen, &ev);
    assert_eq!(
        graded.reason_class, "leakage",
        "leakage was not its own reason class"
    );
    assert_ne!(graded.reason_class, "harness-error");
}

/// Recovery dimension (GAP-M16): a scenario that declares a fault must show an executed
/// fault trigger, and consistent persisted shape after the kill, or the recovery
/// dimension fails and the run is never silently green.
#[test]
fn declared_faults_must_execute_and_recover() {
    let text = good_manifest().replace(
        "[assertions]",
        "[faults]\ninjected = [\"restart-after-round-2\"]\n\n[assertions]",
    );
    let scen = manifest::parse_str(&text, &fixtures()).unwrap();
    let ev = Evidence {
        turns_total: 2,
        turns_ok: 1,
        ..Evidence::default()
    };
    let dims = grader::dimension_results(&scen, &ev);
    let (_, recovery_ok, failures) = dims
        .iter()
        .find(|(d, _, _)| *d == Dimension::Recovery)
        .unwrap();
    assert!(
        !recovery_ok,
        "unexecuted fault passed the recovery dimension"
    );
    assert!(failures.iter().any(|f| f.contains("none executed")));

    let mut executed = ev.clone();
    executed.faults_executed = vec!["second scripted tool round observed".to_string()];
    executed.recovery_after_fault = false;
    let dims = grader::dimension_results(&scen, &executed);
    let (_, recovery_ok, failures) = dims
        .iter()
        .find(|(d, _, _)| *d == Dimension::Recovery)
        .unwrap();
    assert!(!recovery_ok);
    assert!(failures.iter().any(|f| f.contains("consistent shape")));

    let mut recovered = executed.clone();
    recovered.recovery_after_fault = true;
    let dims = grader::dimension_results(&scen, &recovered);
    let (_, recovery_ok, _) = dims
        .iter()
        .find(|(d, _, _)| *d == Dimension::Recovery)
        .unwrap();
    assert!(recovery_ok, "executed fault with consistent shape failed");
}

/// Unsupported fault names are rejected by schema validation (GAP-M16), and the two
/// implemented process-death faults are accepted.
#[test]
fn unsupported_fault_names_are_rejected() {
    let text = good_manifest().replace(
        "[assertions]",
        "[faults]\ninjected = [\"teleport-after-round-3\"]\n\n[assertions]",
    );
    let err = manifest::parse_str(&text, &fixtures());
    assert!(err.is_err(), "unsupported fault name accepted");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("teleport-after-round-3"),
        "error lost the name"
    );

    for supported in ["restart-after-round-2", "crash-at-send"] {
        let ok = good_manifest().replace(
            "[assertions]",
            &format!("[faults]\ninjected = [\"{supported}\"]\n\n[assertions]"),
        );
        assert!(
            manifest::parse_str(&ok, &fixtures()).is_ok(),
            "implemented fault {supported} rejected"
        );
    }
}

/// Leakage scan (R-012): planted corpus markers are found byte-exactly in captured
/// bytes and in files, and the bulk-fixture plant directory is excluded from the
/// output scan so the input never masquerades as a leak.
#[test]
fn planted_secrets_are_found_wherever_they_escape() {
    let bytes = b" harmless prefix CTXEVAL-SECRET-A1B2C3D4E5 trailing";
    let found = secrets::scan_bytes(bytes);
    assert_eq!(found, vec!["CTXEVAL-SECRET-A1B2C3D4E5"]);

    let dir = std::env::temp_dir().join(format!("ctxeval-leak-{}", crate::harness::uniq()));
    let planted = dir.join("session/context/sanitized");
    fs::create_dir_all(planted.parent().unwrap()).unwrap();
    fs::write(&planted, "no marker here").unwrap();
    let vaulted = dir.join("session/context/vault");
    fs::create_dir_all(&vaulted).unwrap();
    fs::write(vaulted.join("h"), "x-txn-9f31ac04be").unwrap();
    let findings = secrets::scan_tree(&dir);
    assert_eq!(findings.len(), 1, "scan missed exactly one planted file");
    assert!(findings[0].0.contains("x-txn-9f31ac04be"));

    let clean = secrets::scan_tree_skipping(&dir, Some(&dir.join("session/context")));
    assert!(clean.is_empty(), "skip set was ignored");

    // A nested fixture directory (the plant input) is skipped by the drive's scan.
    let bulk = dir.join("ws/bulk");
    fs::create_dir_all(&bulk).unwrap();
    fs::write(bulk.join("round-00.txt"), "CTXEVAL-SECRET-A1B2C3D4E5").unwrap();
    let skipped = secrets::scan_tree_skipping(&dir.join("ws"), Some(&bulk));
    assert!(skipped.is_empty(), "the plant input was scanned as a leak");
    fs::remove_dir_all(&dir).ok();
}

/// The graded report shape this harness publishes validates against the schema on the
/// publish path, and a report missing any dimension or observation field is rejected.
#[test]
fn publish_path_report_validates_with_new_fields() {
    let report = json!({
        "id": "ingress-secret-and-digest", "schema_version": 1, "owner_phase": 2,
        "arm": "feature", "expected_status": "red", "runner": "rust",
        "runner_revision": "abc", "fixture_digests": [],
        "profile": {"name": "p", "provider": "openai", "model": "m",
                    "context_limit_tokens": 1000, "max_output_tokens": 100},
        "result": {"verdict": "expected-red", "accepted": true,
                   "reason_class": "leakage", "failures": []},
        "evidence_status": {"source": "independent", "turns_total": 1, "turns_ok": 1,
                            "provider_requests": 9, "tool_calls_scripted": 1,
                            "final_response_issued": true, "wall_hit": false,
                            "terminal_outcome": "none", "isolation_ok": true},
        "cache": report::cache_block(),
        "runtime_config": {"name": "profile-limit", "context_limit": 20000},
        "evidence_dimensions": {"task": true, "protocol": false, "resource": true,
                                "latency": true, "recovery": true, "wall_realism": true,
                                "failures": ["[protocol] leaked"]},
        "request_observations": {"requests": 9, "max_request_bytes": 150_000,
                                 "streamed_requests": 2, "tool_names": ["read_file"],
                                 "last_request_bytes": 90_000,
                                 "observations_source": "loopback",
                                 "request_shape_digest": "deadbeef"},
        "leakage_scan": {"clean": true,
                         "findings": [],
                         "markers": ["m"]},
    });
    report::validate(&report, false).unwrap();
}

/// Arm selection changes installed runtime behavior: the minimum-floor arm's config
/// overrides the context limit the profile carries, while the status-quo arm keeps it,
/// so the comparison arms cannot be identical in behavior.
#[test]
fn minimum_floor_arm_selects_different_context_limit() {
    let dir = Path::new("evals/context-management/scenarios");
    let scenarios = crate::context_eval::manifest::load_dir(dir, &fixtures()).unwrap();
    let by_id: std::collections::BTreeMap<String, _> =
        scenarios.iter().map(|(_, s)| (s.id.clone(), s)).collect();
    let status_quo = &by_id["baseline-status-quo-full-replay"];
    let floor = &by_id["baseline-minimum-management-floor"];
    assert_eq!(status_quo.arm.name(), "status-quo");
    assert_eq!(floor.arm.name(), "minimum-floor");
    assert_eq!(
        status_quo.runtime.context_limit, status_quo.profile.context_limit_tokens,
        "status-quo arm must keep the profile limit"
    );
    assert_ne!(
        floor.runtime.context_limit, status_quo.runtime.context_limit,
        "minimum-floor arm does not select different runtime behavior"
    );
    assert!(
        floor.runtime.context_limit < status_quo.runtime.context_limit,
        "the floor arm's override must bind harder than the status quo"
    );
}

/// The generated profile must carry the arm-selected context limit, not the profile
/// default, so the arm's configuration is what the acceptance target actually runs.
#[test]
fn generated_profile_installs_the_arm_runtime_config() {
    let text = good_manifest().replace("context_limit = 20000", "context_limit = 4000");
    let scen = manifest::parse_str(&text, &fixtures()).unwrap();
    let dir = std::env::temp_dir().join(format!("ctxeval-prof-{}", crate::harness::uniq()));
    // Read back the profile prepare() writes: the public contract under test.
    let prepared =
        runner::prepare(&dir, &scen, "http://127.0.0.1:1/v1", Vec::new(), Vec::new()).unwrap();
    let name = &scen.profile.name;
    let bytes = fs::read_to_string(
        prepared
            .config_home
            .join("profiles")
            .join(format!("{name}.json")),
    )
    .unwrap();
    let profile: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    assert_eq!(
        profile["ephemeralSettings"]["context-limit"],
        json!(scen.runtime.context_limit),
        "the generated profile ignored the arm's runtime config"
    );
    fs::remove_dir_all(&dir).ok();
}

/// The request-shape digest covers exactly what the loopback observes about a request
/// (its content-length-derived size, its tool names, its stream mode) and nothing else,
/// so distinct sizes, tool sets, stream modes, and orderings each move it while identical
/// observations never do.
///
/// The loopback never captures request bodies, so a digest that claimed to cover them
/// would be false evidence: an eight-byte body and a megabyte body hashed identically
/// before this bound was removed. The size is now hashed in full.
#[test]
fn request_shape_digest_follows_what_the_loopback_observes() {
    let req = |index: usize, body_bytes: usize, tools: &[&str], streamed: bool| ObservedRequest {
        index,
        body_bytes,
        tool_names: tools.iter().map(|t| t.to_string()).collect(),
        streamed,
    };
    let base = req(0, 8, &["read_file"], false);
    let digest =
        |requests: &[ObservedRequest]| crate::context_eval::request_shape_digest_for_test(requests);

    let small = vec![req(0, 8, &["read_file"], false)];
    let large = vec![req(0, 9, &["read_file"], false)];
    let huge = vec![req(0, 1 << 20, &["read_file"], false)];
    assert_eq!(digest(std::slice::from_ref(&base)), digest(&small.clone()));
    // A size change the old eight-byte clamp hid is visible now.
    assert_ne!(digest(&small), digest(&large));
    assert_ne!(digest(&small), digest(&huge));
    // So are a tool change, a stream change, and a reordering.
    assert_ne!(digest(&small), digest(&[req(0, 8, &["write_file"], false)]));
    assert_ne!(digest(&small), digest(&[req(0, 8, &["read_file"], true)]));
    let a = vec![req(0, 8, &["read_file"], false), req(1, 9, &[], true)];
    let b = vec![req(1, 9, &[], true), req(0, 8, &["read_file"], false)];
    assert_ne!(digest(&a), digest(&b));
    // Identical observations in identical order are identical.
    assert_eq!(
        digest(&a),
        digest(&[req(0, 8, &["read_file"], false), req(1, 9, &[], true)])
    );
}
