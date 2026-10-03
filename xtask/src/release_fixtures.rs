//! Python adversarial suites are subprocess fixtures, never shell interpreters.
use crate::release_support::{checked, Result};
use std::path::Path;
use std::process::Command;

pub fn run(root: &Path, name: &str) -> Result {
    let script = match name {
        "test-vendor-provenance" => "vendor_provenance.py",
        "test-provider-features" => "provider_features.py",
        "test-dependency-inventory" => "dependency_inventory.py",
        "test-release-workflow" => "release_workflow.py",
        "test-source-bundle-verifier" => "source_bundle.py",
        "test-issue1-operator-protocol-runner" => "operator_protocol.py",
        _ => return Err(format!("unknown release fixture: {name}")),
    };
    checked(
        Command::new("python3")
            .arg(root.join("xtask/fixtures").join(script))
            .arg(std::env::current_exe().map_err(|e| e.to_string())?)
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1"),
    )
}
