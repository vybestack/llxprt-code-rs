use super::*;

fn run(
    policy: ShellTimeoutPolicy,
    command: &str,
    requested: Option<u64>,
) -> Result<String, String> {
    let root = tempfile::tempdir().unwrap();
    let mut config = cfg(root.path());
    config.shell.allow_shell = true;
    config.shell.timeouts = policy;
    let mut args: BTreeMap<String, JsonValue> =
        BTreeMap::from([("command".into(), json!(command))]);
    if let Some(seconds) = requested {
        args.insert("timeout_seconds".into(), json!(seconds));
    }
    let (ok, output) = execute_tool(root.path(), "run_shell_command", json!(args), &config);
    if ok {
        Ok(output)
    } else {
        Err(output)
    }
}

#[test]
fn shell_small_default_and_maximum_reach_executor() {
    let second = Some(Duration::from_secs(1));
    for (policy, requested) in [
        (
            ShellTimeoutPolicy {
                default: second,
                maximum: None,
            },
            None,
        ),
        (
            ShellTimeoutPolicy {
                default: None,
                maximum: second,
            },
            Some(10),
        ),
        (
            ShellTimeoutPolicy {
                default: None,
                maximum: second,
            },
            None,
        ),
    ] {
        let error = run(policy, "sleep 2; printf unexpected", requested).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(!error.contains("unexpected"), "{error}");
    }
}

#[test]
fn shell_override_and_disabled_policy_reach_executor() {
    let policy = ShellTimeoutPolicy {
        default: Some(Duration::from_secs(1)),
        maximum: None,
    };
    assert_eq!(
        run(policy, "sleep 2; printf override", Some(4)).unwrap(),
        "override"
    );
    let disabled = ShellTimeoutPolicy {
        default: None,
        maximum: None,
    };
    assert_eq!(
        run(disabled, "sleep 2; printf unlimited", None).unwrap(),
        "unlimited"
    );
    assert!(run(disabled, "sleep 2", Some(1))
        .unwrap_err()
        .contains("timed out"));
    assert!(run(disabled, "printf invalid", Some(0))
        .unwrap_err()
        .contains("positive"));
    assert!(run(disabled, "exit 7", None).is_err());
}

#[test]
fn shell_policy_precedence_does_not_conflate_default_and_maximum() {
    let policy = ShellTimeoutPolicy {
        default: Some(Duration::from_secs(7)),
        maximum: Some(Duration::from_secs(3)),
    };
    assert_eq!(policy.resolve(None), Some(Duration::from_secs(3)));
    assert_eq!(
        policy.resolve(Some(Duration::from_secs(1))),
        Some(Duration::from_secs(1))
    );
    assert_eq!(
        policy.resolve(Some(Duration::from_secs(9))),
        Some(Duration::from_secs(3))
    );
    assert_eq!(
        ShellTimeoutPolicy {
            default: None,
            maximum: None
        }
        .resolve(None),
        None
    );
}

#[test]
fn parsed_profile_shell_policy_controls_execution() {
    let mut value: JsonValue = serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astramedium.json"
    ))
    .unwrap();
    for (default, maximum, requested, succeeds) in [
        (1, -1, None, false),
        (2700, 1, Some(10), false),
        (1, -1, Some(4), true),
        (-1, -1, None, true),
    ] {
        value["ephemeralSettings"]["shell-default-timeout-seconds"] = json!(default);
        value["ephemeralSettings"]["shell-max-timeout-seconds"] = json!(maximum);
        let profile = crate::profile::parse_profile_value(&value, "astramedium").unwrap();
        let result = run(
            profile.ephemeral.shell_timeouts,
            "sleep 2; printf finished",
            requested,
        );
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        if !succeeds {
            assert!(result.unwrap_err().contains("timed out"));
        }
    }
}
