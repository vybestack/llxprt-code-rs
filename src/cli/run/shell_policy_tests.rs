//! Issue #299: the CLI's real agent constructor must not discard Codex shell policy.
//! Only the provider reply is scripted; dispatch, processes, deadlines and persistence
//! use production code. No provider endpoint or real credentials are needed.
use super::*;
use crate::adapter::{ChatBackend, LlmResult, LlmUsage, ToolCall};
use crate::session::{Lifecycle, SessionStore};
use clap::Parser;
use serde_json::json;
use serdes_ai::core::FinishReason;
use std::cell::Cell;

struct ShellReply {
    args: serde_json::Value,
    sent: Cell<bool>,
}

impl ChatBackend for ShellReply {
    fn request<'a>(
        &'a self,
        _requests: &'a [serdes_ai::core::ModelRequest],
        tools: &'a [crate::tools::ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move {
            assert!(tools.iter().any(|tool| tool.name == "run_shell_command"));
            let first = !self.sent.replace(true);
            Ok(LlmResult {
                continuation: None,
                thinking: String::new(),
                text: if first { "" } else { "Done." }.into(),
                calls: if first {
                    vec![ToolCall {
                        id: "shell-policy-call".into(),
                        name: "run_shell_command".into(),
                        args_json: self.args.to_string(),
                    }]
                } else {
                    Vec::new()
                },
                finish_reason: Some(if first {
                    FinishReason::ToolCall
                } else {
                    FinishReason::Stop
                }),
                usage: LlmUsage::default(),
            })
        })
    }
}

#[test]
fn codex_shell_policy_survives_cli_construction_execution_and_reload() {
    // Default and maximum are independent, including omitted settings. The marker
    // is a side-effect witness, not a timing assertion vulnerable to runner load.
    for (default, maximum, requested, succeeds, diagnostic) in [
        (Some(1), Some(-1), None, false, "timed out after 1000 ms"),
        (Some(2700), Some(1), Some(10), false, "effective timeout 1s"),
        (Some(-1), Some(1), None, false, "effective timeout 1s"),
        (None, Some(1), None, false, "effective timeout 1s"),
        (Some(1), Some(-1), Some(4), true, "finished"),
        (Some(1), Some(-1), Some(-1), true, "finished"),
        (Some(-1), Some(-1), None, true, "finished"),
        (Some(-1), Some(-1), Some(0), false, "positive integer"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut profile = shell_profile(default, maximum);
        let args = Args::parse_from(["llxprt-code-rs", "--allow-shell"]);
        let settings = crate::settings::resolve(crate::settings::SettingsLayers {
            config_root: root.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();
        apply_runtime_settings(&mut profile, &settings).unwrap();
        let mut call = json!({"command":"sleep 2; printf finished > marker; printf finished"});
        if let Some(seconds) = requested {
            call["timeout_seconds"] = json!(seconds);
        }
        let constructed = crate::model_api::registry::ConstructedBackend {
            backend: Box::new(ShellReply {
                args: call,
                sent: Cell::new(false),
            }),
            secret_values: Vec::new(),
            context_limit: profile.ephemeral.context_limit,
            max_rounds: 4,
        };
        let agent =
            build_agent(&args, &profile, &settings, constructed, root.path(), None).unwrap();
        let id = SessionId::parse("issue299-shell-policy").unwrap();
        let store = SessionStore::load_at(&id, root.path()).unwrap();
        let reserved = store
            .start_request(
                None,
                None,
                "Run the shell command and summarize.",
                root.path(),
            )
            .unwrap();
        let run = agent.run(&store, &reserved).unwrap();
        assert_eq!(run.summary, "Done.");
        drop(store);
        let reloaded = SessionStore::load_at(&id, root.path()).unwrap();
        let state = reloaded.snapshot().unwrap();
        let branch = &state.branches[0];
        assert_eq!(branch.lifecycle, Lifecycle::Completed);
        assert_eq!(branch.rounds.len(), 2);
        let result = &branch.rounds[0].calls[0];
        assert_eq!(result.name, "run_shell_command");
        assert_eq!(result.id, "shell-policy-call");
        assert!(!result.refused);
        assert_eq!(
            result.ok, succeeds,
            "{default:?}/{maximum:?}/{requested:?}: {result:?}"
        );
        assert!(result.result.contains(diagnostic), "{result:?}");
        assert_eq!(root.path().join("marker").exists(), succeeds, "{result:?}");
    }
}

fn shell_profile(default: Option<i64>, maximum: Option<i64>) -> Profile {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astramedium.json"
    ))
    .unwrap();
    let ephemeral = value["ephemeralSettings"].as_object_mut().unwrap();
    for (key, seconds) in [
        ("shell-default-timeout-seconds", default),
        ("shell-max-timeout-seconds", maximum),
    ] {
        if let Some(seconds) = seconds {
            ephemeral.insert(key.into(), json!(seconds));
        } else {
            ephemeral.remove(key);
        }
    }
    crate::profile::parse_profile_value(&value, "astramedium").unwrap()
}
