//! Operator-authorized, bounded, value-free protocol evidence runner.
use crate::release_support::{digest, text, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_CAPTURE: u64 = 262144;
const FAILURE: &str = "OPERATOR_PROTOCOL_RUNNER_FAILED";
const ACCOUNT: &str = "interop-test";
const FIXTURE: &str = "llxprt-code-rs-issue1-interop-fixture-v1";
const NODE: &str = r#"
const path = require('node:path');
const { createRequire } = require('node:module');
async function main() {
  const requireFromSibling = createRequire(path.join(process.env.LLXPRT_SIBLING_CHECKOUT, 'package.json'));
  const namespace = requireFromSibling('@napi-rs/keyring');
  const AsyncEntry = namespace.AsyncEntry ?? namespace.default?.AsyncEntry;
  if (typeof AsyncEntry !== 'function') process.exit(20);
  const entry = new AsyncEntry(process.env.LLXPRT_KEYRING_SERVICE, process.env.LLXPRT_KEYRING_ACCOUNT);
  if (process.env.LLXPRT_KEYRING_OPERATION === 'prepare') {
    if ((await entry.getPassword()) !== null) process.exit(21);
    await entry.setPassword(process.env.LLXPRT_KEYRING_FIXTURE);
    if ((await entry.getPassword()) !== process.env.LLXPRT_KEYRING_FIXTURE) process.exit(22);
    return;
  }
  let deletionFailed = false;
  try { await entry.deleteCredential(); } catch { deletionFailed = true; }
  if ((await entry.getPassword()) !== null || deletionFailed) process.exit(23);
}
main().catch(() => process.exit(24));
"#;

struct Mode {
    name: String,
    test: &'static str,
    markers: &'static [&'static str],
}
impl Mode {
    fn parse(value: &str) -> Result<Self> {
        let (test, markers): (&str, &[&str]) = match value {
            "interop" => (
                "disposable_keychain_interop",
                &["INTEROP_OK", "INTEROP_FAILED"],
            ),
            "preflight" => (
                "fixed_item_attributes_preflight",
                &["PREFLIGHT_OK", "PREFLIGHT_PRECONDITION_FAILED"],
            ),
            "shape" => (
                "fixed_item_credential_shape",
                &[
                    "SHAPE_OK",
                    "SHAPE_PRECONDITION_FAILED",
                    "SHAPE_INCOMPATIBLE",
                ],
            ),
            "smoke" => (
                "codex_stateless_two_round_smoke",
                &[
                    "SMOKE_PROTOCOL_ACCEPTED",
                    "SMOKE_STATE_REQUIRED",
                    "SMOKE_PROTOCOL_REJECTED",
                    "SMOKE_INFRASTRUCTURE_FAILURE",
                    "SMOKE_MODEL_NONCOMPLIANT",
                    "SMOKE_PRECONDITION_FAILED",
                ],
            ),
            _ => return Err(FAILURE.into()),
        };
        Ok(Self {
            name: value.into(),
            test,
            markers,
        })
    }
}
fn external(checkout: &Path, path: &Path) -> bool {
    !path.starts_with(checkout) && !checkout.starts_with(path)
}
fn absolute(raw: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() || raw.contains(['\n', '\r']) {
        return Err(FAILURE.into());
    }
    Ok(path)
}
fn variable(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| FAILURE.into())
}
fn directory(raw: &str) -> Result<PathBuf> {
    let path = absolute(raw)?.canonicalize().map_err(|_| FAILURE)?;
    if !path.is_dir() {
        return Err(FAILURE.into());
    }
    Ok(path)
}
fn external_file(root: &Path, raw: &str) -> Result<PathBuf> {
    let path = absolute(raw)?;
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Err(FAILURE.into());
    }
    let parent = path
        .parent()
        .ok_or(FAILURE)?
        .canonicalize()
        .map_err(|_| FAILURE)?;
    if !external(root, &parent) {
        return Err(FAILURE.into());
    }
    Ok(path)
}
fn random() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| FAILURE)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    text(Command::new("git").current_dir(root).args(args)).map(|s| s.trim_end_matches('\n').into())
}
fn clean(root: &Path) -> Result<String> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    if directory(&top)? != root {
        return Err(FAILURE.into());
    }
    let head = git(root, &["rev-parse", "--verify", "HEAD"])?;
    if head.len() != 40
        || !head
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FAILURE.into());
    }
    if !git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        return Err(FAILURE.into());
    }
    Ok(head)
}
fn results(work: &Path, mode: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(work).map_err(|_| FAILURE)? {
        let entry = entry.map_err(|_| FAILURE)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(&format!("issue1-{mode}."))
        {
            continue;
        }
        let status = entry.path().join("status.env");
        if !status.is_file() {
            continue;
        }
        let text = fs::read_to_string(status).map_err(|_| FAILURE)?;
        result.push(
            text.lines()
                .filter_map(|s| s.strip_prefix("RESULT="))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    Ok(result)
}
fn prerequisite(work: &Path, mode: &str) -> Result {
    let required = match mode {
        "interop" => return Ok(()),
        "preflight" => ("interop", "INTEROP_OK"),
        "shape" => ("preflight", "PREFLIGHT_OK"),
        "smoke" => ("shape", "SHAPE_OK"),
        _ => return Err(FAILURE.into()),
    };
    let prior = results(work, required.0)?;
    if !prior.iter().any(|s| s == required.1)
        || (mode == "smoke" && prior.iter().any(|s| s == "SHAPE_INCOMPATIBLE"))
    {
        return Err(FAILURE.into());
    }
    if mode == "smoke" {
        smoke_budget(&results(work, "smoke")?)?;
    }
    Ok(())
}
fn smoke_budget(results: &[String]) -> Result {
    let (mut infrastructure, mut model) = (0, 0);
    for result in results {
        match result.as_str() {
            "SMOKE_INFRASTRUCTURE_FAILURE" => infrastructure += 1,
            "SMOKE_MODEL_NONCOMPLIANT" => model += 1,
            "SMOKE_PRECONDITION_FAILED" => (),
            _ => return Err(FAILURE.into()),
        }
    }
    if infrastructure + model >= 3 || infrastructure >= 2 || model >= 2 {
        return Err(FAILURE.into());
    }
    Ok(())
}

struct Evidence {
    mode_dir: PathBuf,
    watchdog: PathBuf,
    config: PathBuf,
    sibling: Option<PathBuf>,
    service: String,
    prepared: bool,
    finalized: bool,
}
impl Evidence {
    fn watchdog(&self) -> Command {
        let mut command = Command::new(&self.watchdog);
        command.arg("--config").arg(&self.config).arg("--");
        command
    }
    fn keyring(&self, operation: &str) -> Result {
        let mut command = self.watchdog();
        command
            .arg("node")
            .env("LLXPRT_KEYRING_OPERATION", operation)
            .env("LLXPRT_KEYRING_SERVICE", &self.service)
            .env("LLXPRT_KEYRING_ACCOUNT", ACCOUNT)
            .env("LLXPRT_KEYRING_FIXTURE", FIXTURE)
            .env(
                "LLXPRT_SIBLING_CHECKOUT",
                self.sibling.as_ref().ok_or(FAILURE)?,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| FAILURE)?;
        let write = child
            .stdin
            .take()
            .ok_or(FAILURE)?
            .write_all(NODE.as_bytes());
        let status = child.wait().map_err(|_| FAILURE)?;
        write.map_err(|_| FAILURE)?;
        if status.success() {
            Ok(())
        } else {
            Err(FAILURE.into())
        }
    }
    fn cleanup_interop(&mut self) -> Result {
        if self.prepared {
            self.keyring("cleanup")?;
            self.prepared = false;
        }
        Ok(())
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        if self.cleanup_interop().is_err() {
            eprintln!("{FAILURE}");
        }
        if !self.finalized {
            for name in ["test-output.capture", "result.marker", "status.env"] {
                let path = self.mode_dir.join(name);
                if path.exists() {
                    let _ = fs::remove_file(path);
                }
            }
            let _ = fs::remove_dir(&self.mode_dir);
        }
    }
}

pub fn run(root: &Path, args: &[String]) -> Result {
    let _cancellation =
        crate::release_cancellation::Cancellation::install().map_err(|_| FAILURE)?;
    execute(root, args).map_err(|_| FAILURE.to_owned())
}
fn execute(root: &Path, args: &[String]) -> Result {
    let [mode] = args else {
        return Err(FAILURE.into());
    };
    let mode = Mode::parse(mode)?;
    if text(Command::new("uname").arg("-s"))?.trim() != "Darwin"
        || variable("LLXPRT_ISSUE1_OPERATOR_PROTOCOL")? != "I_UNDERSTAND"
    {
        return Err(FAILURE.into());
    }
    let checkout = root.canonicalize().map_err(|_| FAILURE)?;
    let head = clean(&checkout)?;
    let evidence = directory(&variable("LLXPRT_EVIDENCE_ROOT")?)?;
    if !external(&checkout, &evidence) {
        return Err(FAILURE.into());
    }
    let watchdog = external_file(&checkout, &variable("LLXPRT_WATCHDOG")?)?;
    if fs::metadata(&watchdog)
        .map_err(|_| FAILURE)?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err(FAILURE.into());
    }
    let config = external_file(&checkout, &variable("LLXPRT_WATCHDOG_CONFIG")?)?;
    let work = evidence.join("work");
    let target = evidence.join("cargo-target");
    fs::create_dir_all(&work)
        .and_then(|()| fs::create_dir_all(&target))
        .map_err(|_| FAILURE)?;
    prerequisite(&work, &mode.name)?;
    let target = target.canonicalize().map_err(|_| FAILURE)?;
    if !external(&checkout, &target) {
        return Err(FAILURE.into());
    }
    let mode_dir = work.join(format!("issue1-{}.{}", mode.name, random()?));
    fs::create_dir(&mode_dir).map_err(|_| FAILURE)?;
    fs::set_permissions(&mode_dir, fs::Permissions::from_mode(0o700)).map_err(|_| FAILURE)?;
    let mut state = Evidence {
        mode_dir,
        watchdog,
        config,
        sibling: None,
        service: String::new(),
        prepared: false,
        finalized: false,
    };
    if mode.name == "interop" {
        let sibling = directory(&variable("LLXPRT_SIBLING_CHECKOUT")?)?;
        if !external(&checkout, &sibling)
            || !sibling.join("package.json").is_file()
            || !sibling.join("node_modules/@napi-rs/keyring").is_dir()
        {
            return Err(FAILURE.into());
        }
        state.sibling = Some(sibling);
        state.service = format!("llxprt-code-rs-issue1-test-{}", random()?);
        state.prepared = true;
        state.keyring("prepare")?;
    }
    let result = run_test(&checkout, &target, &evidence, &state, &mode)?;
    state.cleanup_interop()?;
    if clean(&checkout)? != head {
        return Err(FAILURE.into());
    }
    record(&mut state, &mode, &head, &result)?;
    if result != mode.markers[0] {
        return Err(FAILURE.into());
    }
    println!("OPERATOR_PROTOCOL_RECORDED");
    Ok(())
}
fn run_test(
    root: &Path,
    target: &Path,
    evidence: &Path,
    state: &Evidence,
    mode: &Mode,
) -> Result<String> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (reader, writer) = UnixStream::pair().map_err(|_| FAILURE)?;
    let stderr: OwnedFd = writer.try_clone().map_err(|_| FAILURE)?.into();
    let stdout: OwnedFd = writer.into();
    let test = format!("model_api::operator_protocol::tests::{}", mode.test);
    let marker = state.mode_dir.join("result.marker");
    let mut command = state.watchdog();
    command
        .current_dir(root)
        .args([
            "cargo",
            "+1.88.0",
            "test",
            "--offline",
            "--locked",
            "--lib",
            &test,
            "--",
            "--ignored",
            "--exact",
            "--test-threads=1",
        ])
        .env("CARGO_TARGET_DIR", target)
        .env("LLXPRT_OPERATOR_RESULT_FILE", &marker)
        .env(
            "LLXPRT_OPERATOR_SESSION_LABEL",
            format!("issue1-smoke-{}", random()?),
        )
        .env("LLXPRT_EVIDENCE_ROOT", evidence)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if state.prepared {
        command
            .env("LLXPRT_OPERATOR_INTEROP_SERVICE", &state.service)
            .env("LLXPRT_OPERATOR_INTEROP_ACCOUNT", ACCOUNT);
    }
    reader
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .map_err(|_| FAILURE)?;
    let mut child = command.spawn().map_err(|_| FAILURE)?;
    drop(command);
    let mut capture = Vec::new();
    let read = capture_output(reader, &mut capture);
    if read.is_err() {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
    }
    // The required external watchdog bounds and reaps its Cargo child.
    let status = child.wait().map_err(|_| FAILURE)?;
    read?;
    if !status.success() || capture.len() as u64 > MAX_CAPTURE {
        return Err(FAILURE.into());
    }
    let capture_text = String::from_utf8_lossy(&capture);
    if capture_text
        .lines()
        .filter(|l| *l == "running 1 test")
        .count()
        != 1
        || capture_text
            .lines()
            .filter(|l| l.contains(&format!("test {test} ... ok")))
            .count()
            != 1
    {
        return Err(FAILURE.into());
    }
    if !fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file()) {
        return Err(FAILURE.into());
    }
    let result = fs::read_to_string(&marker).map_err(|_| FAILURE)?;
    if !result.ends_with('\n')
        || result.lines().count() != 1
        || !mode.markers.contains(&result.trim_end_matches('\n'))
    {
        return Err(FAILURE.into());
    }
    fs::write(state.mode_dir.join("test-output.capture"), capture).map_err(|_| FAILURE)?;
    Ok(result.trim_end_matches('\n').into())
}
fn capture_output(mut reader: std::os::unix::net::UnixStream, capture: &mut Vec<u8>) -> Result {
    let mut buffer = [0; 8192];
    loop {
        crate::release_cancellation::check()?;
        let remaining = (MAX_CAPTURE + 1).saturating_sub(capture.len() as u64) as usize;
        if remaining == 0 {
            return Err(FAILURE.into());
        }
        let bound = remaining.min(buffer.len());
        match reader.read(&mut buffer[..bound]) {
            Ok(0) => return Ok(()),
            Ok(count) => capture.extend_from_slice(&buffer[..count]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(FAILURE.into()),
        }
    }
}

fn record(state: &mut Evidence, mode: &Mode, head: &str, result: &str) -> Result {
    let capture = state.mode_dir.join("test-output.capture");
    let hash = digest(&capture)?;
    let bytes = fs::metadata(&capture).map_err(|_| FAILURE)?.len();
    let marker = state.mode_dir.join("result.marker");
    fs::write(&marker, head).map_err(|_| FAILURE)?;
    let identity = digest(&marker)?;
    fs::remove_file(capture)
        .and_then(|()| fs::remove_file(marker))
        .map_err(|_| FAILURE)?;
    let status = state.mode_dir.join("status.env");
    fs::write(&status, format!("MODE={}\nRESULT={result}\nCARGO_STATUS=0\nCAPTURE_BYTES={bytes}\nCAPTURE_SHA256={hash}\nCHECKOUT_IDENTITY_SHA256={identity}\nCLEANUP_STATUS=OK\n", mode.name)).map_err(|_| FAILURE)?;
    fs::set_permissions(status, fs::Permissions::from_mode(0o444))
        .and_then(|()| fs::set_permissions(&state.mode_dir, fs::Permissions::from_mode(0o555)))
        .map_err(|_| FAILURE)?;
    state.finalized = true;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn smoke_retry_classes_remain_bounded() {
        assert!(smoke_budget(&[]).is_ok());
        assert!(smoke_budget(&[
            "SMOKE_INFRASTRUCTURE_FAILURE".into(),
            "SMOKE_MODEL_NONCOMPLIANT".into()
        ])
        .is_ok());
        for value in [
            "SMOKE_PROTOCOL_ACCEPTED",
            "SMOKE_STATE_REQUIRED",
            "SMOKE_PROTOCOL_REJECTED",
            "unknown",
        ] {
            assert!(smoke_budget(&[value.into()]).is_err());
        }
        for value in ["SMOKE_INFRASTRUCTURE_FAILURE", "SMOKE_MODEL_NONCOMPLIANT"] {
            assert!(smoke_budget(&[value.into(), value.into()]).is_err());
        }
    }
}
