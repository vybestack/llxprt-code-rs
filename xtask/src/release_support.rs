//! Shared release subprocess and private-directory ownership.
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Result<T = ()> = std::result::Result<T, String>;

pub fn checked(command: &mut Command) -> Result {
    crate::release_cancellation::check()?;
    let status = command.status().map_err(|e| format!("{command:?}: {e}"))?;
    crate::release_cancellation::check()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?}: {status}"))
    }
}

pub fn output(command: &mut Command) -> Result<Output> {
    let output = command.output().map_err(|e| format!("{command:?}: {e}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{command:?}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn text(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?.stdout).map_err(|e| e.to_string())
}

pub fn python(root: &Path, script: &str) -> Command {
    let mut command = Command::new("python3");
    command
        .arg(root.join("scripts").join(script))
        .current_dir(root)
        .env("PYTHONDONTWRITEBYTECODE", "1");
    command
}

pub fn cargo(root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command.arg("+1.88.0").current_dir(root);
    command
}

pub fn digest(path: &Path) -> Result<String> {
    let value =
        text(Command::new("shasum").args([OsStr::new("-a"), OsStr::new("256"), path.as_os_str()]))?;
    let digest = value.split_whitespace().next().ok_or("missing SHA-256")?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid SHA-256 output".into());
    }
    Ok(digest.to_owned())
}

pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new(root: &Path, prefix: &str) -> Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("tmp"));
        fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        for _ in 0..100 {
            let path = base.join(format!(
                "{prefix}.{}.{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            use std::os::unix::fs::DirBuilderExt;
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("private directory allocation exhausted".into())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            eprintln!("remove {}: {e}", self.0.display());
        }
    }
}
