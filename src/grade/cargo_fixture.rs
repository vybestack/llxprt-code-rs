//! Standalone manifests and bounded diagnostics for grader-owned Cargo packages.
use super::{process, CmdSpec, CONSUMER_TIMEOUT};
use std::path::Path;

/// Copies are independent packages, not members of the source or temporary ancestor workspace.
/// Keep an existing local workspace (and its dependency declarations), but drop a relocated
/// package's explicit pointer to its old workspace. Unresolved inheritance still fails closed.
pub(super) fn write_manifest(base: &Path, text: &str) -> bool {
    let result = (|| -> Result<(), String> {
        let mut manifest: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let root = manifest.as_table_mut().ok_or("manifest is not a table")?;
        if let Some(package) = root.get_mut("package").and_then(toml::Value::as_table_mut) {
            package.remove("workspace");
        }
        root.entry("workspace")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let text = toml::to_string(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(base.join("Cargo.toml"), text).map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            eprintln!("grader Cargo fixture setup at {}: {error}", base.display());
            false
        }
    }
}

pub(super) fn run(program: &str, base: &Path, max_output: usize) -> Option<process::CmdOutcome> {
    match process::run_cmd(CmdSpec {
        program: program.to_string(),
        args: vec![
            "test".to_string(),
            "--offline".to_string(),
            "--manifest-path".to_string(),
            "./Cargo.toml".to_string(),
        ],
        cwd: Some(base.to_path_buf()),
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: Some(CONSUMER_TIMEOUT),
        max_output,
    }) {
        Ok(outcome) => Some(outcome),
        Err(error) => {
            eprintln!(
                "grader {program} test --offline --manifest-path ./Cargo.toml at {}: could not run: {error}",
                base.display()
            );
            None
        }
    }
}

/// Diagnostic text is bounded independently of the runner's capture budget. Preserve separate
/// stream tails, status, timeout and truncation flags rather than reducing the failure to false.
pub(super) fn diagnostic(base: &Path, outcome: &process::CmdOutcome) -> String {
    fn tail(bytes: &[u8]) -> String {
        let start = bytes.len().saturating_sub(4096);
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }
    format!(
        "grader cargo test --offline --manifest-path ./Cargo.toml at {}: status={:?}, timed_out={}, stdout_truncated={}, stderr_truncated={}, combined_truncated={}\nstdout tail:\n{}\nstderr tail:\n{}",
        base.display(), outcome.status, outcome.timed_out,
        outcome.stdout_truncated, outcome.stderr_truncated, outcome.combined_truncated,
        tail(&outcome.stdout), tail(&outcome.stderr)
    )
}
