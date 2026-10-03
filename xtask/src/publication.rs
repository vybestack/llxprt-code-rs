//! Atomic immutable release creation. No draft, asset mutation, or resume operation.
use crate::release_support::{checked, digest, output, text, Result, Temp};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

type Environment = BTreeMap<&'static str, String>;
fn required(env: &mut Environment, names: &[&'static str]) -> Result {
    for name in names {
        let value = std::env::var(name).map_err(|_| format!("{name} is required"))?;
        if value.is_empty() {
            return Err(format!("{name} is required"));
        }
        env.insert(name, value);
    }
    Ok(())
}
fn api(args: &[&str]) -> Result<String> {
    text(Command::new("gh").arg("api").args(args))
}
fn tag(env: &Environment) -> Result {
    let endpoint = format!(
        "repos/{}/git/ref/tags/{}",
        env["GITHUB_REPOSITORY"], env["RELEASE_TAG"]
    );
    if api(&[&endpoint, "--jq", ".object.type"])?.trim() != "tag" {
        return Err("release ref is not an annotated tag".into());
    }
    let sha = api(&[&endpoint, "--jq", ".object.sha"])?;
    let endpoint = format!("repos/{}/git/tags/{}", env["GITHUB_REPOSITORY"], sha.trim());
    if api(&[&endpoint, "--jq", ".object.type"])?.trim() != "commit" {
        return Err("annotated release tag does not point directly to a commit".into());
    }
    if api(&[&endpoint, "--jq", ".object.sha"])?.trim() != env["EXPECTED_COMMIT"] {
        return Err("remote release tag does not match the workflow commit".into());
    }
    Ok(())
}
fn jq(value: &str, args: &[&str]) -> Result<String> {
    let mut child = Command::new("jq")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("jq stdin missing")?
        .write_all(value.as_bytes())
        .map_err(|e| e.to_string())?;
    let result = child.wait_with_output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "JSON policy rejected: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    String::from_utf8(result.stdout).map_err(|e| e.to_string())
}
const RULE: &str = r#"
.target == "tag" and .enforcement == "active" and has("bypass_actors") and
(.bypass_actors | type == "array" and length == 0) and
((has("current_user_can_bypass") | not) or .current_user_can_bypass == "never") and
((.conditions.ref_name.exclude // []) | length == 0) and
((.conditions.ref_name.include // []) | any(. == $ref or . == "~ALL")) and
(([.rules[]?.type] | index("update")) != null) and
(([.rules[]?.type] | index("deletion")) != null)
"#;
fn remote_policy(env: &Environment) -> Result {
    let base = format!("repos/{}", env["GITHUB_REPOSITORY"]);
    let immutable = api(&[&format!("{base}/immutable-releases")])?;
    jq(&immutable, &["-e", ".enabled == true"])
        .map_err(|_| "immutable releases are not enabled")?;
    let rules = api(&[
        "--paginate",
        "--slurp",
        &format!("{base}/rulesets?targets=tag&per_page=100"),
    ])?;
    let ids = jq(
        &rules,
        &[
            "-r",
            "flatten[]? | select(.target == \"tag\" and .enforcement == \"active\") | .id",
        ],
    )?;
    let mut accepted = false;
    for id in ids.lines() {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("GitHub returned an invalid tag ruleset id".into());
        }
        let rule = api(&[&format!("{base}/rulesets/{id}")])?;
        if jq(
            &rule,
            &[
                "-e",
                "--arg",
                "ref",
                &format!("refs/tags/{}", env["RELEASE_TAG"]),
                RULE,
            ],
        )
        .is_ok()
        {
            accepted = true;
            break;
        }
    }
    if !accepted {
        return Err("the release tag lacks an active no-bypass update/deletion ruleset".into());
    }
    let existing = api(&[
        "--paginate",
        "--slurp",
        &format!("{base}/releases?per_page=100"),
    ])?;
    // Parse failures are failures, not evidence that no release exists.
    let exists = jq(
        &existing,
        &[
            "-r",
            "--arg",
            "tag",
            &env["RELEASE_TAG"],
            "flatten | any(.tag_name == $tag)",
        ],
    )?;
    if exists.trim() != "false" {
        return Err("a release or draft already exists for the release tag".into());
    }
    Ok(())
}
fn durable_identity(env: &Environment, cwd: &Path) -> Result<String> {
    let dist = cwd.join("dist");
    checked(
        Command::new("sha256sum")
            .current_dir(&dist)
            .args(["--check", &env["RELEASE_SIDECAR"]]),
    )?;
    let sidecar =
        fs::read_to_string(dist.join(&env["RELEASE_SIDECAR"])).map_err(|e| e.to_string())?;
    let lines: Vec<_> = sidecar.lines().collect();
    if lines.len() != 1 {
        return Err("release checksum sidecar must contain exactly one entry".into());
    }
    let line = lines[0];
    let hash = line
        .split_whitespace()
        .next()
        .ok_or("missing release digest")?;
    if !hex_digest(hash) {
        return Err("release checksum sidecar has an invalid digest".into());
    }
    if line[hash.len()..].trim_start().trim_start_matches('*') != env["RELEASE_ARCHIVE"] {
        return Err("release checksum sidecar names another archive".into());
    }
    let sidecar_digest = digest(&dist.join(&env["RELEASE_SIDECAR"]))?;
    if !env["SOURCE_OCI_MANIFEST_DIGEST"]
        .strip_prefix("sha256:")
        .is_some_and(hex_digest)
    {
        return Err("OCI source manifest digest is invalid".into());
    }
    let base = format!(
        "https://ghcr.io/v2/{}-source",
        env["GITHUB_REPOSITORY"].to_lowercase()
    );
    for (name, expected) in [
        (
            "SOURCE_OCI_MANIFEST_URL",
            format!("{base}/manifests/{}", env["SOURCE_OCI_MANIFEST_DIGEST"]),
        ),
        (
            "SOURCE_OCI_ARCHIVE_URL",
            format!("{base}/blobs/sha256:{hash}"),
        ),
        (
            "SOURCE_OCI_SIDECAR_URL",
            format!("{base}/blobs/sha256:{sidecar_digest}"),
        ),
    ] {
        if env[name] != expected {
            return Err(format!(
                "{name} does not name the verified digest-qualified GHCR object"
            ));
        }
    }
    Ok(hash.to_owned())
}
fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn run(root: &Path, args: &[String]) -> Result {
    let mut env = Environment::new();
    required(
        &mut env,
        &[
            "GH_TOKEN",
            "GITHUB_REPOSITORY",
            "RELEASE_TAG",
            "EXPECTED_COMMIT",
        ],
    )?;
    if args == ["--verify-tag-only"] {
        return tag(&env);
    }
    if !args.is_empty() {
        return Err("unexpected publisher argument".into());
    }
    required(
        &mut env,
        &[
            "RELEASE_ARCHIVE",
            "RELEASE_SIDECAR",
            "GITHUB_SERVER_URL",
            "GITHUB_RUN_ID",
            "SOURCE_OCI_MANIFEST_DIGEST",
            "SOURCE_OCI_MANIFEST_URL",
            "SOURCE_OCI_ARCHIVE_URL",
            "SOURCE_OCI_SIDECAR_URL",
        ],
    )?;
    let temp = Temp::new(root, "llxprt-release")?;
    remote_policy(&env)?;
    tag(&env)?;
    let hash = durable_identity(&env, &std::env::current_dir().map_err(|e| e.to_string())?)?;
    let body = format!("The source bundle, checksum sidecar, and OCI manifest have GitHub build provenance attestations.\n\nWorkflow evidence: {}/{}/actions/runs/{}\n\nArchive: `{}`\nArchive SHA-256: `{hash}`\nDurable archive: {}\nDurable checksum sidecar: {}\nOCI manifest: {}\nOCI manifest digest: `{}`", env["GITHUB_SERVER_URL"], env["GITHUB_REPOSITORY"], env["GITHUB_RUN_ID"], env["RELEASE_ARCHIVE"], env["SOURCE_OCI_ARCHIVE_URL"], env["SOURCE_OCI_SIDECAR_URL"], env["SOURCE_OCI_MANIFEST_URL"], env["SOURCE_OCI_MANIFEST_DIGEST"]);
    let metadata = [
        "--arg",
        "tag",
        &env["RELEASE_TAG"],
        "--arg",
        "commit",
        &env["EXPECTED_COMMIT"],
        "--arg",
        "name",
        &env["RELEASE_TAG"],
        "--arg",
        "body",
        &body,
    ];
    let payload = output(Command::new("jq").arg("-n").args(metadata).arg("{tag_name:$tag, target_commitish:$commit, name:$name, body:$body, draft:false, prerelease:false, generate_release_notes:false, make_latest:\"true\"}"))?;
    let payload_path = temp.0.join("payload.json");
    fs::write(&payload_path, payload.stdout).map_err(|e| e.to_string())?;
    let created = api(&[
        "--method",
        "POST",
        &format!("repos/{}/releases", env["GITHUB_REPOSITORY"]),
        "--input",
        payload_path.to_str().ok_or("non-UTF-8 payload path")?,
    ])?;
    let mut args = vec!["-e"];
    args.extend(metadata);
    args.push(".tag_name == $tag and .target_commitish == $commit and .name == $name and .body == $body and .draft == false and .prerelease == false and .immutable == true and ((.discussion_url? // \"\") == \"\") and (.assets | length == 0)");
    jq(&created, &args)
        .map_err(|_| "created release does not match the immutable publication contract")?;
    tag(&env)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_is_lowercase_exact_width() {
        assert!(hex_digest(&"a".repeat(64)));
        for value in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
            assert!(!hex_digest(&value));
        }
    }
}
