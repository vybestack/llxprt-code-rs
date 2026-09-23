use super::*;

#[test]
fn compiled_cli_lists_dot_at_explicit_cwd() {
    let (address, requests, server) =
        spawn_responses_tool_server("list_directory", r#"{"path":"."}"#);
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("root-evidence.txt"), "root").unwrap();
    std::fs::create_dir(workspace.path().join("nested-evidence")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let profiles = home.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    std::fs::write(
        profiles.join("directory.json"),
        serde_json::json!({
            "provider": "openai-responses",
            "model": "loopback-responses",
            "ephemeralSettings": {
                "base-url": format!("http://{address}/v1/responses"),
                "api-key": "loopback-test-key"
            }
        })
        .to_string(),
    )
    .unwrap();
    let output = bin()
        .env("LLXPRT_CONFIG_HOME", home.path())
        .current_dir(home.path())
        .args([
            "--profile",
            "directory",
            "--session",
            "directory-root",
            "--cwd",
        ])
        .arg(workspace.path())
        .args(["-p", "List the project root using list_directory path dot."])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    let bodies = requests.recv().unwrap();
    assert_eq!(bodies.len(), 2);
    let tool = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "list_directory")
        .expect("list_directory schema must reach the provider");
    let help = tool["parameters"]["properties"]["path"]["description"]
        .as_str()
        .unwrap();
    assert!(help.contains("\".\""), "{help}");

    let result = bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call-1")
        .expect("CLI must return the directory tool result to the provider");
    let listing = result["output"].as_str().unwrap();
    println!("compiled CLI list_directory path dot: {listing}");
    assert!(listing.contains("file root-evidence.txt"), "{listing}");
    assert!(listing.contains("dir nested-evidence"), "{listing}");
    assert!(!listing.contains("not reachable"), "{listing}");
    assert!(
        !listing.contains("profiles"),
        "must list --cwd, not process cwd: {listing}"
    );
}
