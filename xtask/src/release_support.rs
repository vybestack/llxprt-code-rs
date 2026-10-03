//! Shared release subprocess and private-directory ownership.
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub type Result<T = ()> = std::result::Result<T, String>;

/// The retained scope leader reserves the group identity until all signalling is done.
/// Never signal a command PID after try_wait: it may already have been reaped/reused.
pub(crate) struct ReleaseChild {
    pub(crate) child: Child,
    scope: Option<crate::release_scope::Scope>,
    pub(crate) termination_grace: Duration,
}

impl ReleaseChild {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self> {
        crate::release_cancellation::check()?;
        let scope = crate::release_scope::Scope::new()?;
        scope.configure(command);
        let child = command.spawn().map_err(|e| format!("{command:?}: {e}"))?;
        Ok(Self {
            child,
            scope: Some(scope),
            termination_grace: Duration::from_millis(500),
        })
    }

    pub(crate) fn wait(&mut self) -> Result<ExitStatus> {
        loop {
            if let Err(cancelled) = crate::release_cancellation::check() {
                self.terminate()?;
                return Err(cancelled);
            }
            match self.child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => {
                    // The reserved leader still exists even though try_wait reaped the child.
                    // End the scope before waiting for captured pipes held by descendants.
                    drop(self.scope.take());
                    return Ok(status);
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn reap_until(&mut self, duration: Duration) -> Result<bool> {
        let deadline = Instant::now() + duration;
        loop {
            if self.child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate(&mut self) -> Result {
        if let Some(scope) = &self.scope {
            // Only the owned, unreaped scope leader reserves this process-group identity.
            if unsafe { libc::kill(-scope.pgid, libc::SIGTERM) } != 0 {
                return Err(format!(
                    "terminate release scope: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // Give helpers (notably the publisher) time to unwind their own private state.
            let reaped = self.reap_until(self.termination_grace)?;
            // Scope::drop kills descendants even if the command leader exited first.
            drop(self.scope.take());
            if !reaped && !self.reap_until(Duration::from_secs(2))? {
                return Err("release child did not reap after SIGKILL".into());
            }
        }
        Ok(())
    }
}

impl Drop for ReleaseChild {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            // Still destroy the retained scope on an error; never leave ownership live.
            drop(self.scope.take());
            eprintln!("release child cleanup failed: {error}");
        }
    }
}

pub fn checked(command: &mut Command) -> Result {
    let status = ReleaseChild::spawn(command)?.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?}: {status}"))
    }
}

pub fn output(command: &mut Command) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ReleaseChild::spawn(command)?;
    let stdout = child.child.stdout.take().ok_or("release stdout missing")?;
    let stderr = child.child.stderr.take().ok_or("release stderr missing")?;
    let capture = |mut stream: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let stdout = capture(Box::new(stdout));
    let stderr = capture(Box::new(stderr));
    let status = child.wait();
    // On a wait error Drop also ends the scope before joining the pipe readers.
    drop(child);
    let collect = |reader: thread::JoinHandle<std::io::Result<Vec<u8>>>| {
        reader
            .join()
            .map_err(|_| "release output reader panicked".to_string())?
            .map_err(|e| e.to_string())
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);
    let output = Output {
        status: status?,
        stdout: stdout?,
        stderr: stderr?,
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_pipes_close_when_command_leader_exits_before_descendant() {
        let started = Instant::now();
        let result = output(Command::new("python3").args([
            "-c",
            "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); print('owned output')",
        ])).expect("capture owned output");
        assert_eq!(result.stdout, b"owned output\n");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn captured_stdout_and_stderr_drain_concurrently() {
        let result = output(Command::new("python3").args([
            "-c",
            "import sys; sys.stdout.write('o'*131072); sys.stderr.write('e'*131072)",
        ]))
        .expect("drain both pipes");
        assert_eq!(result.stdout, vec![b'o'; 131072]);
        assert_eq!(result.stderr, vec![b'e'; 131072]);
    }
}
