use super::*;

#[test]
fn list_directory_dot_lists_root_and_nested_with_independent_offsets() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("root.txt"), "root").unwrap();
    std::fs::create_dir_all(d.path().join("child/deep")).unwrap();
    std::fs::write(d.path().join("child/deep/nested.txt"), "nested").unwrap();
    let config = cfg(d.path());
    for path in [".", "", "child", "child/deep", ".", "", "child/deep"] {
        let (ok, body) = execute_tool(d.path(), "list_directory", json!({"path": path}), &config);
        assert!(ok, "list {path:?}: {body}");
        let expected = match path {
            "child" => "dir deep",
            "child/deep" => "file nested.txt",
            _ => "file root.txt",
        };
        assert!(body.contains(expected), "list {path:?}: {body}");
    }
}

#[test]
fn directory_roots_reject_escapes_and_symlink_traversal() {
    use std::os::unix::fs::symlink;

    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("child/deep")).unwrap();
    std::fs::create_dir(outside.path().join("deep")).unwrap();
    std::fs::write(d.path().join("plain.txt"), "needle").unwrap();
    std::fs::write(outside.path().join("secret.txt"), "needle outside").unwrap();
    symlink(outside.path(), d.path().join("escape")).unwrap();
    symlink("child", d.path().join("inside")).unwrap();
    symlink(".", d.path().join("rootlink")).unwrap();
    symlink("missing", d.path().join("dangling")).unwrap();
    symlink(outside.path(), d.path().join("child/escape")).unwrap();
    symlink("deep", d.path().join("child/inside")).unwrap();

    for path in [
        "..",
        "../",
        "./..",
        "./../secret.txt",
        "child/..",
        "child/../../",
        "child/../child",
        "/",
        outside.path().to_str().unwrap(),
        "escape",
        "escape/deep",
        "child/escape",
        "inside",
        "inside/deep",
        "child/inside",
        "rootlink",
        "rootlink/child",
        "dangling",
        "plain.txt",
        "\0",
        ".\0",
        "./escape",
        "./",
        "./.",
        "./child",
    ] {
        for tool in ["list_directory", "search_file_content"] {
            let args = if tool == "list_directory" {
                json!({"path": path})
            } else {
                json!({"path": path, "pattern": "needle"})
            };
            let (ok, body) = run(d.path(), tool, args);
            assert!(!ok, "{tool} must reject {path:?}: {body}");
            assert!(!body.contains("needle outside"), "{body}");
        }
    }
    let (ok, body) = run(d.path(), "list_directory", json!({"path": "."}));
    assert!(ok, "{body}");
    for name in ["escape", "inside", "rootlink", "dangling"] {
        assert!(body.contains(&format!("symlink {name}")), "{body}");
    }
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"path": ".", "pattern": "needle"}),
    );
    assert!(ok, "{body}");
    assert!(body.contains("plain.txt"), "{body}");
    assert!(!body.contains("secret.txt"), "{body}");
}

#[test]
fn directory_root_representation_does_not_relax_file_paths() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("child")).unwrap();
    for path in [
        ".",
        "",
        "child",
        "./new.txt",
        "../new.txt",
        "child/../new.txt",
    ] {
        for (tool, args) in [
            ("read_file", json!({"path": path})),
            ("write_file", json!({"path": path, "content": "changed"})),
            (
                "replace",
                json!({"path": path, "old_string": "old", "new_string": "new"}),
            ),
        ] {
            let (ok, body) = run(d.path(), tool, args);
            assert!(!ok, "{tool} must reject {path:?}: {body}");
        }
    }
    assert!(d.path().join("child").is_dir());
    assert!(!d.path().join("new.txt").exists());
    assert!(
        run(
            d.path(),
            "write_file",
            json!({"path": "child/normal.txt", "content": "normal"})
        )
        .0
    );
    let (ok, body) = run(d.path(), "read_file", json!({"path": "child/normal.txt"}));
    assert!(ok, "{body}");
    assert!(body.contains("normal"), "{body}");
}

#[test]
fn directory_dot_uses_retained_capability_after_root_path_swap() {
    let d = tempfile::tempdir().unwrap();
    let original = d.path().join("workspace");
    let moved = d.path().join("moved");
    let outside = d.path().join("outside");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(original.join("pinned.txt"), "needle pinned").unwrap();
    std::fs::write(outside.join("outside.txt"), "needle outside").unwrap();
    let config = cfg(&original);
    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&outside, &original).unwrap();
    for _ in 0..2 {
        for (tool, args) in [
            ("list_directory", json!({"path": "."})),
            (
                "search_file_content",
                json!({"path": ".", "pattern": "needle"}),
            ),
        ] {
            let (ok, body) = execute_tool(&original, tool, args, &config);
            assert!(ok, "{tool}: {body}");
            assert!(body.contains("pinned.txt"), "{body}");
            assert!(!body.contains("outside.txt"), "{body}");
        }
    }
}

#[test]
fn search_directory_root_and_nested_paths_have_independent_offsets() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("child/deep")).unwrap();
    std::fs::write(d.path().join("root.txt"), "needle root").unwrap();
    std::fs::write(d.path().join("child/deep/nested.txt"), "needle nested").unwrap();
    let config = cfg(d.path());
    for path in [Some("."), None, Some(""), Some("child/deep"), Some(".")] {
        let mut args = json!({"pattern": "needle"});
        if let Some(path) = path {
            args["path"] = json!(path);
        }
        let (ok, body) = execute_tool(d.path(), "search_file_content", args, &config);
        assert!(ok, "search {path:?}: {body}");
        assert!(body.contains("nested.txt"), "{body}");
        assert_eq!(
            body.contains("root.txt"),
            path != Some("child/deep"),
            "{body}"
        );
    }
}

#[test]
fn directory_path_schema_documents_executable_root_and_nested_paths() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src/tools")).unwrap();
    for spec in tool_specs(false)
        .into_iter()
        .filter(|spec| matches!(spec.name.as_str(), "list_directory" | "search_file_content"))
    {
        let schema = crate::adapter::schema_for(&spec).parameters_json_schema;
        let path_schema = &schema["properties"]["path"];
        let help = path_schema["description"].as_str().unwrap();
        assert!(help.contains("\".\""), "{help}");
        assert!(help.contains("\"\""), "{help}");
        assert!(help.contains("symlink"), "{help}");
        assert!(help.contains(".."), "{help}");
        let validator = jsonschema::validator_for(&schema).unwrap();
        for path in [".", "", "src/tools"] {
            let mut args = json!({"path": path});
            if spec.name == "search_file_content" {
                args["pattern"] = json!("needle");
            }
            assert!(validator.is_valid(&args), "{}: {args}", spec.name);
            let (ok, body) = run(d.path(), &spec.name, args);
            assert!(ok, "{} {path:?}: {body}", spec.name);
        }
        assert!(!validator.is_valid(&json!({"path": 1})));
        if spec.name == "list_directory" {
            assert!(!validator.is_valid(&json!({})));
        }
    }
}
