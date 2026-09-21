use super::grader_file;

// Bind Cargo's library and required integration-test declarations to the files
// inspected by the grader. Reject target suppression rather than accepting a
// successful build of a decoy. Explicit equivalent declarations remain valid.
pub(super) fn intended_cargo_targets(ws: &crate::tools::WorkspaceCap) -> bool {
    let Some(manifest) = grader_file(ws, "Cargo.toml") else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&manifest) else {
        return false;
    };
    let enabled = |target: &toml::Value| {
        ["test", "harness"]
            .iter()
            .all(|key| target.get(key).and_then(toml::Value::as_bool) != Some(false))
            && target.get("required-features").is_none()
    };
    if let Some(lib) = value.get("lib") {
        if lib
            .get("path")
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs")
            != "src/lib.rs"
            || !enabled(lib)
        {
            return false;
        }
    } else if value
        .get("package")
        .and_then(|p| p.get("autolib"))
        .and_then(toml::Value::as_bool)
        == Some(false)
    {
        return false;
    }
    let explicit = value.get("test").and_then(toml::Value::as_array);
    let roundtrip = explicit.and_then(|tests| {
        tests
            .iter()
            .find(|t| t.get("name").and_then(toml::Value::as_str) == Some("roundtrip"))
    });
    if let Some(test) = roundtrip {
        if test
            .get("path")
            .and_then(toml::Value::as_str)
            .unwrap_or("tests/roundtrip.rs")
            != "tests/roundtrip.rs"
            || !enabled(test)
        {
            return false;
        }
    } else if value
        .get("package")
        .and_then(|p| p.get("autotests"))
        .and_then(toml::Value::as_bool)
        == Some(false)
    {
        return false;
    }
    grader_file(ws, "tests/roundtrip.rs").is_some()
}
