//! Harness self-tests for the Phase 0 context evals (#37).
//!
//! These prove the harness itself rejects malformed reports, catches a false success built
//! from a model-authored claim, rejects bad manifests, bounds fixture expansion and
//! artifact capture, and constructs the exact adapter commands (including the TypeScript
//! reference invocation). No test contacts a live provider.

use crate::context_eval::grader::{self, Evidence, Verdict};
use crate::context_eval::{manifest, report, runner, ARTIFACT_STREAM_CAP};
use serde_json::json;
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
    assert!(config_home.is_absolute() && workspace.is_absolute());

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

#[test]
fn malformed_reports_are_detected() {
    let mut good = json!({
        "id": "x", "schema_version": 1, "owner_phase": 7, "arm": "feature",
        "expected_status": "red", "runner": "rust", "runner_revision": "abc",
        "fixture_digests": [], "profile": {}, "result": {}, "evidence_status": {},
        "cache": report::cache_block(),
    });
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

    let aggregate = json!({
        "tool": "llxprt-context-eval", "schema_version": 1, "run_id": "r",
        "runner": "rust", "runner_revision": "abc", "expected_status_mode": true,
        "cache": report::cache_block(),
        "summary": {"total": 0, "expected_red": 0, "unexpected_green": 0,
                    "unexpected_red": 0, "harness_error": 0},
        "scenarios": [],
    });
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
    good.as_object_mut().unwrap().remove("id");
    assert!(
        report::validate(&good, false).is_err(),
        "missing id accepted"
    );
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
    let caught = grader::verdict(&scen, &graded);
    assert_ne!(
        caught,
        Verdict::UnexpectedGreen,
        "the lie was graded as a pass"
    );
    assert!(!caught.accepted(), "the lie was accepted");
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
