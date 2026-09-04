//! Mid-invocation fault injection for the context evals (GAP-M16, R-013).
//!
//! Two of the three declared faults are process deaths: the restart fault `SIGKILL`s the
//! acceptance target's whole process group at the second scripted tool round, and the
//! crash fault kills it while a provider request is in flight. The third makes the
//! session's own context store unwritable mid-invocation, so the store's later writes
//! really fail with `EACCES` rather than succeeding through an already-open handle. All
//! three are applied to a run this harness created, and all three are reverted when the
//! drive ends so no fault outlives its scenario.

use crate::context_eval::faults::{self, MidRunFault};
use crate::context_eval::loopback;
use crate::harness;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Poll interval shared by every fault thread.
const FAULT_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Deadline for a fault trigger or the faulted child's death, so a dead run cannot hang
/// the drive.
const FAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Scripted tool rounds the restart fault waits for (the second scripted call sent).
const RESTART_TOOL_ROUNDS: usize = 2;
/// Deadline for the store's first write: bounded, so a dead run cannot hang the drive.
const FAULT_POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// An armed mid-run process-death fault: the kill target the drive stops at the end, and
/// the thread that reported the executed trigger.
pub struct ArmedFault {
    pub target: KillTarget,
    pub handle: std::thread::JoinHandle<Option<String>>,
}

/// Hand-off between the turn loop's spawn wrapper and the fault thread.
#[derive(Clone)]
pub struct KillTarget {
    /// File the spawn wrapper writes its own pid to (the child becomes the acceptance
    /// target through `exec`, so this is exactly the process-group leader pid).
    pub pid_file: PathBuf,
    /// Set when the drive is over, so the thread never kills a later scenario's run.
    pub stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Loopback observation state; restart fires on the second scripted tool call.
    pub observations: std::sync::Arc<std::sync::Mutex<loopback::Observations>>,
}

/// Install the mid-run process-death fault a scenario selected, if it selected one.
///
/// The bounded runner spawns a wrapper that registers its own pid and then becomes the
/// acceptance target through `exec`, so the fault thread's `SIGKILL` reaches the real
/// run in place while the runner's own capture and validation stay unchanged.
pub fn arm_mid_run_fault(
    scen: &crate::context_eval::manifest::Scenario,
    cli: &Path,
    out_dir: &Path,
    shared_observations: std::sync::Arc<std::sync::Mutex<loopback::Observations>>,
) -> Result<Option<ArmedFault>, String> {
    let Some(fault) = faults::mid_run_fault(&scen.faults.injected) else {
        return Ok(None);
    };
    fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let pid_file = out_dir.join("child.pid");
    let wrapper = out_dir.join("cli-wrapper.sh");
    write_spawn_wrapper(&wrapper, cli, &pid_file)
        .map_err(|e| format!("write spawn wrapper for fault {}: {e}", fault.name()))?;
    std::env::set_var("LLXPRT_CODE_RS_BIN", wrapper.display().to_string());
    let target = KillTarget {
        pid_file,
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        observations: shared_observations,
    };
    let thread_target = target.clone();
    let handle = std::thread::spawn(move || run_kill_fault(fault, thread_target));
    Ok(Some(ArmedFault { target, handle }))
}

/// `SIGKILL` the whole process group led by `pid`. The acceptance target runs as its own
/// session leader, so a negative pid reaches every descendant, not just the direct child.
fn kill_process_group(pid: u32) -> bool {
    if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } == 0 {
        return true;
    }
    unsafe { libc::kill(pid as i32, libc::SIGKILL) == 0 }
}

/// Poll until `kill(pid, 0)` reports the pid is gone (bounded: a not-yet-reaped zombie
/// only delays this, never hangs it).
fn wait_pid_gone(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + FAULT_TIMEOUT;
    loop {
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(FAULT_POLL_INTERVAL);
    }
}

/// Read the process-group leader pid the spawn wrapper registered, if it is there yet.
fn registered_pid(pid_file: &Path) -> Option<u32> {
    let text = fs::read_to_string(pid_file).ok()?;
    text.trim().parse::<u32>().ok()
}

/// Whether the fault's trigger point has been observed at the loopback.
fn fault_armed(fault: MidRunFault, observations: &loopback::Observations) -> bool {
    match fault {
        MidRunFault::Restart => observations.tool_calls_issued >= RESTART_TOOL_ROUNDS,
        MidRunFault::Crash => !observations.requests.is_empty(),
    }
}

/// Kill the acceptance target's process group at the fault's trigger point. Returns the
/// executed trigger description, or `None` when the drive ended or the trigger never
/// arrived (which the recovery dimension reports as no executed fault).
fn run_kill_fault(fault: MidRunFault, target: KillTarget) -> Option<String> {
    let deadline = std::time::Instant::now() + FAULT_TIMEOUT;
    loop {
        if target.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let armed = fault_armed(
            fault,
            &target
                .observations
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        if armed {
            if let Some(pid) = registered_pid(&target.pid_file) {
                let killed = kill_process_group(pid);
                let gone = wait_pid_gone(pid);
                if killed && gone {
                    harness::eprint_status(&format!(
                        "context-evals fault executed: {} killed process group {pid} ({})",
                        fault.name(),
                        fault.trigger()
                    ));
                    return Some(fault.trigger().to_string());
                }
                harness::eprint_status(&format!(
                    "context-evals fault {} could not confirm the death of pid {pid}",
                    fault.name()
                ));
                return None;
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(FAULT_POLL_INTERVAL);
    }
}

/// Spawn wrapper the faulted drives install as `LLXPRT_CODE_RS_BIN`: it registers its own
/// pid for the fault thread and then becomes the acceptance target through `exec`, so
/// the bounded runner, envelope validation, and continuation checks are all unchanged.
pub fn write_spawn_wrapper(path: &Path, cli: &Path, pid_file: &Path) -> Result<(), String> {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nexec '{}' \"$@\"\n",
        pid_file.display(),
        cli.display()
    );
    fs::write(path, script).map_err(|e| format!("write {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    Ok(())
}

/// One scenario's session `context/` directory, resolved from the harness-owned config
/// home so the fault can only ever land inside a run this harness created.
pub struct StoreUnwritableGuard<'a> {
    dir: &'a Path,
    session: &'a str,
}

impl<'a> StoreUnwritableGuard<'a> {
    /// Bind the guard to this scenario's own session context directory. Fault selection
    /// is validated upstream by `faults::validate` and requested via
    /// `faults::wants_store_unwritable`.
    pub fn new(
        scen: &'a crate::context_eval::manifest::Scenario,
        prepared: &'a crate::context_eval::runner::Prepared,
    ) -> Option<Self> {
        if !faults::wants_store_unwritable(&scen.faults.injected) {
            return None;
        }
        Some(Self {
            dir: &prepared.config_home,
            session: &prepared.session,
        })
    }

    /// The session `context/` directory this guard's fault will target.
    pub fn context_dir(&self) -> PathBuf {
        self.dir
            .join("code-rs-sessions")
            .join(self.session)
            .join("context")
    }
}

/// Mid-invocation fault injection for the unwritable-store scenario.
///
/// This scenario is driven as ONE CLI invocation, so no turn boundary exists for a chmod
/// to land on: a turn-anchored fault fires only after the invocation that should have
/// observed it already finished. A side thread instead polls for the first appearance of
/// `context/manifest.json` and then makes the directory and every file in it unwritable
/// (`0o500` / `0o400`), so the store's own later writes actually fail with EACCES rather
/// than succeeding through an already-open handle.
pub struct StoreUnwritableInjection {
    handle: Option<std::thread::JoinHandle<bool>>,
    context: PathBuf,
}

impl StoreUnwritableInjection {
    /// Polls for the first store artifact, applies the fault, and reports it on stderr.
    /// The thread returns whether the fault was actually applied.
    pub fn start(guard: &StoreUnwritableGuard<'_>) -> Self {
        let context = guard.context_dir();
        let handle = std::thread::spawn({
            let context = context.clone();
            move || inject_unwritable(context)
        });
        Self {
            handle: Some(handle),
            context,
        }
    }

    /// Whether the injection thread reported the fault as applied.
    pub fn applied(&mut self) -> bool {
        match self.handle.take() {
            Some(handle) => handle.join().unwrap_or(false),
            None => false,
        }
    }
}

impl Drop for StoreUnwritableInjection {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // The fault must never outlive this scenario's own drive: restore the modes the
        // store expects (`0o700` dir / `0o600` files) so later phases read a usable store.
        restore_writable(&self.context);
    }
}

/// Waits for `context/manifest.json`, then makes `context/` and its files unwritable.
///
/// Returns once the fault is applied (or the deadline passes, which the scenario's own
/// verdict reports as missing evidence).
fn inject_unwritable(context: PathBuf) -> bool {
    let manifest = context.join("manifest.json");
    let deadline = std::time::Instant::now() + FAULT_POLL_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if manifest.is_file() {
            break;
        }
        std::thread::sleep(FAULT_POLL_INTERVAL);
    }
    if !manifest.is_file() {
        return false;
    }
    // File modes first: a read-only directory alone does not stop a write that re-opens
    // an existing `0o600` file with O_TRUNC through an open directory handle.
    if let Ok(entries) = fs::read_dir(&context) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o400));
            }
        }
    }
    if fs::set_permissions(&context, fs::Permissions::from_mode(0o500)).is_ok() {
        harness::eprint_status("context-evals fault: session context store made unwritable");
        return true;
    }
    false
}

/// Restores the session `context/` tree so later phases read a clean, usable store.
fn restore_writable(context: &Path) {
    if let Ok(entries) = fs::read_dir(context) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600));
            }
        }
    }
    let _ = fs::set_permissions(context, fs::Permissions::from_mode(0o700));
}

/// Files a consistent session `context/` store must carry. A store that is missing them
/// is not a store, so consistency is judged against existence first.
pub const STORE_FILES: [&str; 3] = ["manifest.json", "events.log", "rewrite-journal.log"];

/// Consistent-shape check for the session `context/` store after a fault-triggered
/// process death.
///
/// Every expected file must **exist**: an absent store is an inconsistent store, not a
/// clean one, so the earlier "when present" shape let a vanished store pass as recovered.
/// The manifest must parse as one JSON object, and every line-framed artifact must end on
/// a frame boundary, so a restart replays whole frames only and never a torn tail.
pub fn store_shape_consistent(context: &Path) -> bool {
    if !context.is_dir() {
        return false;
    }
    for name in STORE_FILES {
        if !context.join(name).is_file() {
            return false;
        }
    }
    let manifest = context.join("manifest.json");
    match fs::read(&manifest) {
        Ok(bytes) => {
            if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                return false;
            }
        }
        Err(_) => return false,
    }
    // `events.log` and `rewrite-journal.log` are newline-separated JSON documents,
    // atomically published (tmp file + rename), so a torn frame here is not expected
    // from a crash; every document must parse. The vault and sanitized spine are
    // binary/framed formats the replaying process itself validates on open.
    for name in ["events.log", "rewrite-journal.log"] {
        let path = context.join(name);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(line).is_err() {
                return false;
            }
        }
    }
    true
}
