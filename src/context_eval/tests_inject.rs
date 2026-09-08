//! Self-tests for the fault-injection machinery in `src/context_eval/inject.rs`
//! (GAP-M16, R-013). Split from the parent self-test module only to respect the
//! per-file LOC gate; the imports mirror the parent module's.
//!
//! The three declared faults are exercised directly and hermetically: the
//! unwritable-store injection makes a real `context/` tree unwritable and restores it,
//! the consistent-shape check rejects absent and incomplete stores, the spawn wrapper
//! registers its own pid before becoming the target through `exec` (so the kill path
//! reaches a real short-lived child), and `arm_mid_run_fault` fires its kill exactly at
//! the round boundary it declares. No test contacts a live provider, runs no harness
//! drive, and depends on nothing outside `/bin/sleep` and the wrapper the machinery
//! itself writes.
//!
//! The three wrapper tests do not touch the wrapper from the library test process at
//! all: each `exec`s a dedicated single-test helper (`std::env::current_exe()` run with
//! `--exact --nocapture`) that performs its whole write / arm / direct-spawn / boundary
//! / kill / reap sequence in its own process. The library test process never owns the
//! wrapper's writer while a concurrently forked sibling test child can still be sitting
//! between its own `fork` and `exec` (issue #264: a `fork`ed sibling inherits a
//! close-on-exec descriptor, so `O_CLOEXEC` is not a fork barrier and the direct wrapper
//! `exec` fails with Linux `ETXTBSY`). `tests_etxtbsy.rs` holds the causal evidence.

use crate::context_eval::faults;
use crate::context_eval::inject::{
    store_shape_consistent, write_spawn_wrapper, StoreUnwritableGuard,
};
use crate::context_eval::loopback;
use crate::context_eval::manifest::{
    Arm, Assertions, ExpectedStatus, Faults, ProfileSpec, RuntimeConfig, Scenario, Stimulus,
    WallSpec,
};
use crate::context_eval::runner::Prepared;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Environment selector that turns this same test binary into the single-test helper
/// for the wrapper-boundary sequence (see the module docs).
pub(crate) const FAULT_SEQUENCE_HELPER: &str = "LLXPRT_TEST_FAULT_SEQUENCE";

/// Environment selector holding the helper's own temp directory. Its value is minted by
/// the test before the helper is spawned, so no two sequences can share one.
pub(crate) const FAULT_SEQUENCE_DIR: &str = "LLXPRT_TEST_FAULT_SEQUENCE_DIR";

/// Supervise one fault-sequence helper to its natural end inside `bound` (see
/// [`supervise_helper_inner`]). `tests_etxtbsy` reuses this for its Linux witness helper,
/// passing its own outer bound that strictly exceeds every budget nested inside that
/// helper; only that Linux-gated caller needs it, so it is compiled only there and never
/// reads as dead code on Darwin.
#[cfg(target_os = "linux")]
pub(crate) fn supervise_helper(child: std::process::Child, bound: Duration) -> HelperRun {
    supervise_helper_inner(child, bound)
}

/// Deadline for one helper's whole supervised lifetime: its verdict, libtest's footer, and
/// its exit all have to arrive inside this bound, so a dead helper path fails the test
/// instead of hanging it. A supervisor that nests another whole supervised helper inside
/// the one it waits on must pass a bound of its own that exceeds the nested one, or the
/// outer deadline expires first and reports a timeout the inner helper never caused.
pub(crate) const HELPER_BOUND: Duration = Duration::from_secs(120);

/// Deadline for one helper's own bounded step (a pid registration, a boundary kill, a
/// confirmed death): a dead path fails the helper instead of hanging it.
pub(crate) const HELPER_STEP_BOUND: Duration = Duration::from_secs(30);

/// `poll(2)` tick while supervising a helper, short enough that the deadline and a kill are
/// noticed promptly and long enough that an idle wait costs nothing.
const HELPER_POLL_TICK_MS: i32 = 10;

/// What supervising one helper produced: its captured stdout, its exit status, whether the
/// deadline had to kill it, and the supervision failure that ended the read loop early, if
/// one did. The failure is carried instead of panicked so the corpse is always reaped and
/// the caller sees the output the helper managed to print.
pub(crate) struct HelperRun {
    pub(crate) output: String,
    pub(crate) status: Option<std::process::ExitStatus>,
    pub(crate) timed_out: bool,
    pub(crate) failure: Option<String>,
}

/// Reap one helper inside `HELPER_STEP_BOUND`, `SIGKILL`ing it at the deadline: no reap in
/// this module blocks forever, and not even a ptraced helper can outlive its bound.
///
/// The post-kill collect is bounded the same way rather than a blocking `wait()`, because
/// a child in uninterruptible sleep ignores even `SIGKILL` and a blocking collect would
/// then hang this process past the very invariant this function exists to enforce. `None`
/// is returned only in that case — a corpse that stayed uncollectable inside a whole
/// second step bound — and the supervisor surfaces it through `HelperRun::failure`; every
/// other path returns a collected status.
fn reap_bounded(child: &mut std::process::Child, what: &str) -> Option<std::process::ExitStatus> {
    let pid = child.id();
    let start = Instant::now();
    let mut killed = false;
    // One loop, two bounds: poll until the corpse arrives, `SIGKILL` at the first step
    // bound, then keep polling until a second step bound covers the post-kill collect.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(error) => panic!("reap {what} {pid}: {error}"),
        }
        let elapsed = start.elapsed();
        if !killed {
            if elapsed < HELPER_STEP_BOUND {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            // Past the bound the helper is killed and its corpse is collected on the same
            // bounded polling pattern, so even a wedged or ptraced helper cannot hang here.
            let _ = child.kill();
            killed = true;
            continue;
        }
        if elapsed >= HELPER_STEP_BOUND + HELPER_STEP_BOUND {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Why supervision left its read loop: the helper closed its stdout, the deadline fired, or
/// one real `poll`/`read` failure.
enum Stop {
    /// EOF: the helper and every process it spawned are done writing.
    Eof,
    /// `HELPER_BOUND` (or the caller's own bound) elapsed before EOF.
    Deadline,
    /// `poll` failed with a real errno (not `EINTR`).
    Poll(String),
    /// Reading the piped stdout failed with a real errno (not `EAGAIN`/`EINTR`).
    Read(String),
}

/// Supervise one fault-sequence helper to its natural end: keep reading the piped stdout
/// until the helper closes it (which libtest does only after printing its
/// `test result: ...` footer), then reap the child. Nothing is dropped early, so libtest
/// never writes into a closed pipe and its errors always reach this assertion instead of
/// dying as `EPIPE` (issue #264).
///
/// The bound is real, not decorative: past it the helper is `SIGKILL`ed, the still-open
/// still-open pipe is closed, and the corpse is reaped, so a wedged helper fails the test.
/// Every failure path — the deadline, a failed poll, a failed read, a failed kill — reaps
/// the same way, because an unreaped child would outlive the test as a zombie holding its
/// pid. A poll or read failure is reported to the caller through `HelperRun::failure`
/// rather than panicked, because the pipe is about to be dropped and the status the caller
/// asserts on must be the helper's real one.
fn supervise_helper_inner(mut child: std::process::Child, bound: Duration) -> HelperRun {
    use std::io::Read;

    let mut pipe = child
        .stdout
        .take()
        .expect("the helper's stdout must be piped");
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut pollfd = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = Instant::now() + bound;
    let stop = loop {
        if Instant::now() >= deadline {
            break Stop::Deadline;
        }
        // Safety: `poll` on this process's own open descriptor with a finite timeout.
        let ready = unsafe { libc::poll(&mut pollfd, 1, HELPER_POLL_TICK_MS) };
        if ready == 0 {
            continue;
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Stop::Poll(format!("poll the helper's stdout: {error}"));
        }
        match pipe.read(&mut chunk) {
            Ok(0) => break Stop::Eof,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(error) => break Stop::Read(format!("read the helper's stdout: {error}")),
        }
    };

    let deadline_hit = matches!(stop, Stop::Deadline);
    if deadline_hit {
        let _ = child.kill();
    }
    // Closing the read end cannot strand a well-behaved helper (that way lies libtest's
    // `EPIPE`); the bounded reap below is what actually waits for it.
    drop(pipe);
    let pid = child.id();
    let status = reap_bounded(&mut child, "the fault-sequence helper");
    // The deadline is the only stop that leaves the helper alive, and the reap above
    // `SIGKILL`s it at its own step bound, so the deadline's failure is that the bound
    // itself was exceeded; the other stops all arrive with the helper already finished.
    let failure = match stop {
        Stop::Eof if status.is_none() => Some(format!(
            "the fault-sequence helper {pid} never became collectable after its EOF"
        )),
        Stop::Eof => None,
        Stop::Deadline if status.is_none() => Some(format!(
            "the fault-sequence helper {pid} was killed at its deadline and never became \
             collectable"
        )),
        Stop::Deadline => Some(format!(
            "the fault-sequence helper {pid} was killed at its deadline"
        )),
        Stop::Poll(message) | Stop::Read(message) => Some(message),
    };
    HelperRun {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        status,
        timed_out: deadline_hit,
        failure,
    }
}

/// Bounded wait for a poll-style condition. Every fault thread in the drive polls at a
/// fixed interval, so the tests wait the same way and never hang on a dead path: the
/// probe is retried until it holds or the deadline passes, and the deadline is what
/// makes a dead path fail fast rather than hang.
///
/// The probe is handed the target child it is waiting on, so the timeout panic cannot
/// leave that child as an orphan: it is killed and its corpse collected (see
/// [`kill_and_reap`]) before the panic leaves this process. That is the module's stated
/// invariant for every bounded wait in a helper that spawned the child it waits beside.
fn eventually(
    what: &str,
    deadline: Duration,
    child: &mut std::process::Child,
    mut probe: impl FnMut(&mut std::process::Child) -> bool,
) {
    let start = Instant::now();
    while !probe(child) {
        if start.elapsed() >= deadline {
            kill_and_reap(child);
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Kill a child target and collect its corpse, bounded by [`HELPER_STEP_BOUND`]: the
/// module's invariant is that no child the helper spawned outlives it, not even on a
/// failing path and not even as an unreaped zombie. Used on every path that is about to
/// panic away a still-live child, mirroring what `reap_bounded` does for a supervised
/// helper.
fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let pid = child.id();
    let deadline = Instant::now() + HELPER_STEP_BOUND;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => panic!("reap the wrapper target {pid} after SIGKILL: {error}"),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the wrapper target {pid} would not exit after SIGKILL");
}

/// A fresh tempdir per test, unique so tests can run in parallel.
fn tempdir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ctxeval-{prefix}-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Restore writability first so a failing test still cleans up after itself.
fn cleanup(dir: &Path) {
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    let _ = fs::remove_dir_all(dir);
}

/// A scenario that selects one of the injected faults. The fault machinery reads only
/// `scen.faults.injected`, so a hand-built scenario keeps this test free of a full
/// manifest parse while exercising the real selection path.
fn fault_scenario(injected: &[&str]) -> Scenario {
    Scenario {
        schema_version: 1,
        id: "fault-probe".to_string(),
        owner_phase: 2,
        arm: Arm::StatusQuo,
        expected_status: ExpectedStatus::Red,
        expected_reason_class: "context-limit".to_string(),
        accept_any_reason: false,
        profile: ProfileSpec {
            name: "p".to_string(),
            provider: "openai".to_string(),
            model: "m".to_string(),
            context_limit_tokens: 1000,
            max_output_tokens: 100,
        },
        stimulus: Stimulus {
            prompt: "p".to_string(),
            followups: Vec::new(),
        },
        wall: WallSpec {
            tool_rounds: 1,
            tool_output_bytes: 1024,
            fixture: "f".to_string(),
        },
        assertions: Assertions::default(),
        faults: Faults {
            injected: injected.iter().map(|f| f.to_string()).collect(),
        },
        runtime: RuntimeConfig {
            context_limit: 1000,
            name: "status-quo".to_string(),
        },
    }
}

/// The `Prepared` the guard binds to. Only the config home and session are read on
/// the path under test, so a hand-built one avoids a full `prepare()` drive.
fn prepared(config_home: &Path) -> Prepared {
    Prepared {
        config_home: config_home.to_path_buf(),
        workspace: config_home.join("ws"),
        profile_name: "p".to_string(),
        bulk: Vec::new(),
        fixture_digests: Vec::new(),
        session: "ctxeval-inject-test".to_string(),
    }
}

/// The `context/` tree a mid-run store looks like, carrying every file a consistent
/// store must have. The injection waits for `manifest.json` exactly as the drive does.
fn minimal_store(context: &Path) {
    fs::create_dir_all(context).unwrap();
    for (name, body) in [
        ("manifest.json", "{\"id\":\"s\"}"),
        ("events.log", "{\"e\":1}"),
        ("rewrite-journal.log", "{\"j\":1}"),
    ] {
        fs::write(context.join(name), body).unwrap();
    }
}

/// The unwritable-store fault makes the session `context/` tree really unwritable for
/// the scope it guards, and ending that scope returns the store to a writable, usable
/// shape: a write that succeeds before and after must fail only inside it.
#[test]
fn store_unwritable_injection_blocks_then_restores_writes() {
    let dir = tempdir("store-fault");
    let session = prepared(&dir);
    let scen = fault_scenario(&[faults::STORE_UNWRITABLE]);
    let guard = StoreUnwritableGuard::new(&scen, &session);
    assert!(guard.is_some(), "the selected fault did not arm");
    let guard = guard.unwrap();
    let context = guard.context_dir();
    minimal_store(&context);

    let probe = context.join("events.log");
    let write_probe = || fs::write(&probe, b"").is_ok();
    assert!(write_probe(), "writable store before the fault");

    let mut injection = crate::context_eval::inject::StoreUnwritableInjection::start(&guard);
    // The injection polls for `manifest.json`, which already exists, so `applied()`
    // joins the thread and reports the fault as applied immediately.
    assert!(
        injection.applied(),
        "the unwritable-store fault was never applied"
    );
    assert!(
        fs::write(&probe, b"x").is_err(),
        "a write through the faulted store still succeeded"
    );
    assert!(
        fs::metadata(&context).unwrap().permissions().mode() & 0o222 == 0,
        "the context directory was not made unwritable"
    );

    // Ending the guarded scope restores the modes the store expects: 0o700 dir, 0o600
    // files, so later phases read a clean, usable store.
    drop(injection);
    assert!(
        write_probe(),
        "the unwritable-store fault outlived its guarded scope"
    );
    assert_eq!(
        fs::metadata(&context).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&probe).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        store_shape_consistent(&context),
        "the restored store is not a consistent store"
    );
    cleanup(&dir);
}

/// A consistent store needs every declared file present and framed: an absent
/// `context/` directory is an inconsistent store, and so is a store missing any
/// required file, while a minimal well-formed store of whole JSON frames is not.
#[test]
fn store_shape_consistency_requires_the_whole_store() {
    let dir = tempdir("store-shape");
    let absent = dir.join("absent");
    assert!(
        !store_shape_consistent(&absent),
        "an absent context directory passed as a consistent store"
    );

    for missing in crate::context_eval::inject::STORE_FILES {
        let partial = dir.join(format!("missing-{missing}"));
        minimal_store(&partial);
        fs::remove_file(partial.join(missing)).unwrap();
        assert!(
            !store_shape_consistent(&partial),
            "a store missing {missing} passed as a consistent store"
        );
    }

    let store = dir.join("store");
    minimal_store(&store);
    assert!(
        store_shape_consistent(&store),
        "a minimal well-formed store was rejected"
    );

    // Every declared artifact must still parse: a torn frame or a non-object manifest is
    // not a store a restart can safely replay.
    let torn = dir.join("torn");
    minimal_store(&torn);
    fs::write(torn.join("events.log"), "{\"e\":1}\n{\"e\":").unwrap();
    assert!(
        !store_shape_consistent(&torn),
        "a torn events frame passed as a consistent store"
    );
    let bad_manifest = dir.join("manifest");
    minimal_store(&bad_manifest);
    fs::write(bad_manifest.join("manifest.json"), "not-json").unwrap();
    assert!(
        !store_shape_consistent(&bad_manifest),
        "a non-object manifest passed as a consistent store"
    );
    cleanup(&dir);
}

/// The spawn wrapper registers its own pid before becoming the acceptance target
/// through `exec`, so the group kill reaches the real target in place and its death is
/// confirmed within a bound. A short-lived `/bin/sleep` child stands in for the
/// acceptance target: hermetic, no network, no harness drive.
///
/// The sequence runs inside the helper process (see the module docs), so the assertions
/// are the helper's own and this test checks the helper's real write / direct-wrapper-
/// exec / pid-registration / group-kill / reap control flow succeeded end to end.
#[test]
fn spawn_wrapper_registers_pid_and_kill_path_confirms_death() {
    run_sequence_helper(&[], "kill-path");
}

/// `arm_mid_run_fault` arms exactly the kill fault a scenario selected, writes the spawn
/// wrapper, and fires at the declared round boundary: the restart fault waits for the
/// second scripted tool round, so one round short it must hold, and at two it must kill
/// the exec'd target's group and confirm the death.
///
/// The whole sequence runs inside the helper process (see the module docs), so the
/// assertions are the helper's own and this test only checks that the helper's real
/// `arm_mid_run_fault` / direct-wrapper-exec / boundary / kill / reap control flow
/// succeeded end to end.
#[test]
fn arm_mid_run_fault_fires_at_the_declared_round_boundary() {
    run_sequence_helper(&[faults::RESTART_AFTER_ROUND_2], "restart-boundary");
}

/// The crash fault's boundary is a provider request in flight, so it must not fire on
/// scripted tool rounds alone: an armed crash fault holds through five rounds and fires
/// the moment a request is observed.
///
/// Same helper topology as the restart boundary test above.
#[test]
fn arm_mid_run_fault_crash_boundary_is_a_request_in_flight() {
    run_sequence_helper(&[faults::CRASH_AT_SEND], "crash-boundary");
}

/// Run one wrapper-boundary sequence in a process of its own and check its verdict.
///
/// The helper re-execs the test binary with `--exact --nocapture` on its own entry
/// point, the same single-test helper convention `tools::tests::cancellation` uses, so
/// its own `println!` verdict reaches the captured stdout. `identity` names the
/// sequence's temp directory, so concurrent sequences never share one: the wrapper, the
/// pid file, and the wrapper's registered pid stay per-sequence and the registered pid
/// is the pid this sequence spawned.
///
/// The helper is supervised to its natural end (see [`supervise_helper`]): its stdout
/// stays open through libtest's closing footer, so a libtest error — a dead helper, an
/// unwritable wrapper, a kill that never landed — surfaces here as a failed helper exit
/// with its own output attached, never as a silent `EPIPE`. The verdict is asserted on
/// the complete output, and the exit status is asserted unweakened: a helper that did
/// not exit 0 fails this test.
fn run_sequence_helper(injected: &[&str], identity: &str) {
    let dir = tempdir(&format!("arm-fault-{identity}"));
    let joined = injected.join(",");
    let expected = if joined.is_empty() {
        "arm-sequence-ok:none".to_string()
    } else {
        format!("arm-sequence-ok:{joined}")
    };
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "context_eval::tests_inject::fault_sequence_helper",
            "--nocapture",
        ])
        .env(FAULT_SEQUENCE_HELPER, &joined)
        .env(FAULT_SEQUENCE_DIR, dir.display().to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn the fault-sequence helper");
    let run = supervise_helper_inner(child, HELPER_BOUND);
    assert!(
        !run.timed_out,
        "the fault-sequence helper for {joined} outlived its bound; output so far:\n{}",
        run.output
    );
    assert!(
        run.failure.is_none(),
        "supervising the fault-sequence helper for {joined} failed: {}\n{}",
        run.failure.unwrap_or_default(),
        run.output
    );
    assert!(
        run.status
            .expect("a failure-free supervisor always reaps")
            .success(),
        "the fault-sequence helper failed for {joined}:\n{}",
        run.output
    );
    assert!(
        run.output.lines().any(|line| line.trim() == expected),
        "the fault-sequence helper for {joined} never reported `{expected}`:\n{}",
        run.output
    );
    cleanup(&dir);
}

/// One helper entry point, driven once per wrapper sequence (`none`, `restart`, then
/// `crash`), so each runs with the wrapper writer opened only inside this process.
#[test]
fn fault_sequence_helper() {
    let Some(injected) = std::env::var_os(FAULT_SEQUENCE_HELPER) else {
        return;
    };
    let injected = injected.to_string_lossy().into_owned();
    let dir = std::env::var_os(FAULT_SEQUENCE_DIR)
        .unwrap_or_else(|| panic!("fault-sequence helper needs its directory"))
        .to_string_lossy()
        .into_owned();
    let dir = PathBuf::from(dir);
    let verdict = match injected.as_str() {
        faults::RESTART_AFTER_ROUND_2 => {
            run_wrapper_boundary_sequence(FaultSequence::Restart, &dir)
        }
        faults::CRASH_AT_SEND => run_wrapper_boundary_sequence(FaultSequence::Crash, &dir),
        "" => run_kill_path_sequence(&dir),
        other => panic!("no wrapper-boundary sequence for fault {other}"),
    };
    println!("{verdict}");
}

/// The declared fault each helper invocation drives, mapped to the observation boundary
/// it must fire at.
enum FaultSequence {
    /// Restart: fire on the second scripted tool round and not one round short.
    Restart,
    /// Crash: hold through five scripted rounds, fire on the first provider request.
    Crash,
}

/// The real wrapper-boundary control flow, in this helper process only.
///
/// This is the sequence that failed as `ETXTBSY` in the library test process: arm the
/// declared fault (which writes and `chmod`s the wrapper), directly `exec` that wrapper
/// as the acceptance target with its argument forwarded, assert the registered pid is
/// the exec'd target, hold at the boundary, kill the group, confirm the death, and reap
/// the child. Every step is bounded, and on a failing assertion the child is killed and
/// reaped before the panic leaves this process. Returns the helper's verdict line.
fn run_wrapper_boundary_sequence(sequence: FaultSequence, dir: &Path) -> String {
    use crate::context_eval::inject;

    let observations = Arc::new(Mutex::new(loopback::Observations::default()));
    let injected = match sequence {
        FaultSequence::Restart => faults::RESTART_AFTER_ROUND_2,
        FaultSequence::Crash => faults::CRASH_AT_SEND,
    };
    let scen = fault_scenario(&[injected]);
    let armed_fault = match inject::arm_mid_run_fault(
        &scen,
        Path::new("/bin/sleep"),
        dir,
        observations.clone(),
    ) {
        Ok(armed) => armed.expect("the declared fault did not arm"),
        Err(error) => panic!("arm the declared fault: {error}"),
    };

    // The wrapper is installed where the bounded runner picks the target up from, and
    // the target this sequence execs through it is a real, short-lived child.
    assert!(dir.join("cli-wrapper.sh").is_file());
    let mut child = spawn_and_confirm_registration(dir, "the fault-sequence target");

    match sequence {
        FaultSequence::Restart => {
            // One scripted round is one short of the declared boundary: no kill yet.
            set_tool_rounds(&observations, 1);
            assert_bound_holds(
                &mut child,
                "restart fault fired before its declared round boundary",
            );
            // At the second scripted round the fault kills the exec'd target's group,
            // confirms the death, and reports the trigger it executed.
            set_tool_rounds(&observations, 2);
        }
        FaultSequence::Crash => {
            // Five scripted rounds with no provider request: the crash fault holds.
            set_tool_rounds(&observations, 5);
            assert_bound_holds(&mut child, "crash fault fired without a request in flight");
            // The boundary is a request in flight: the first observed request fires it.
            observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .requests
                .push(loopback::ObservedRequest {
                    index: 0,
                    body_bytes: 42,
                    tool_names: Vec::new(),
                    streamed: false,
                });
        }
    }

    eventually(
        "the fault to kill the target",
        HELPER_STEP_BOUND,
        &mut child,
        |child| matches!(child.try_wait(), Ok(Some(_))),
    );
    let trigger = armed_fault
        .handle
        .join()
        .ok()
        .flatten()
        .expect("the declared fault never executed");
    let expected = match sequence {
        FaultSequence::Restart => faults::MidRunFault::Restart.trigger(),
        FaultSequence::Crash => faults::MidRunFault::Crash.trigger(),
    };
    assert_eq!(
        trigger, expected,
        "the executed trigger is not the declared one"
    );
    let _ = child.wait();

    // A scenario with no kill fault arms nothing: an undeclared fault must not run.
    let none = inject::arm_mid_run_fault(
        &fault_scenario(&[]),
        Path::new("/bin/sleep"),
        dir,
        observations.clone(),
    );
    match none {
        Ok(armed) => assert!(armed.is_none(), "an undeclared fault armed"),
        Err(error) => panic!("arm an undeclared fault: {error}"),
    }

    format!("arm-sequence-ok:{injected}")
}

/// One scripted tool-round count, as the loopback the fault thread watches records it.
fn set_tool_rounds(observations: &Arc<Mutex<loopback::Observations>>, rounds: usize) {
    observations
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .tool_calls_issued = rounds;
}

/// Directly `exec` the wrapper in `dir` as the acceptance target and confirm the pid it
/// registered is the pid that was spawned. On a mismatch the still-live target is killed
/// and its corpse collected before the failing assertion panics, which is the module's
/// invariant for every child a helper spawns.
fn spawn_and_confirm_registration(dir: &Path, what: &str) -> std::process::Child {
    let mut child = spawn_wrapper_target(dir, "30");
    let registered = registered_pid(dir.join("child.pid"), &mut child);
    if registered != child.id() {
        kill_and_reap(&mut child);
    }
    assert_eq!(
        registered,
        child.id(),
        "{what}: the registered pid is not the exec'd target"
    );
    child
}

/// Spawn the wrapper directly as the acceptance target, with its argument forwarded.
fn spawn_wrapper_target(dir: &Path, arg: &str) -> std::process::Child {
    Command::new(dir.join("cli-wrapper.sh"))
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "direct wrapper exec failed (the writer the arming step opened is held by this \
                 process or an inheriting sibling): {error}"
            )
        })
}

/// Read the pid the wrapper in `dir` registered, bounded by the helper's step bound so
/// a wrapper that never registers fails instead of hanging. The target child is passed
/// in so the same invariant holds on every failing path out of here: it is killed and
/// reaped before the panic leaves this process.
fn registered_pid(pid_file: PathBuf, child: &mut std::process::Child) -> u32 {
    eventually(
        "the wrapper to register its pid",
        HELPER_STEP_BOUND,
        child,
        |_| {
            fs::read_to_string(&pid_file)
                .map(|s| s.trim().parse::<u32>().is_ok())
                .unwrap_or(false)
        },
    );
    fs::read_to_string(&pid_file)
        .unwrap_or_else(|error| {
            kill_and_reap(child);
            panic!("read {}: {error}", pid_file.display())
        })
        .trim()
        .parse()
        .unwrap_or_else(|error| {
            kill_and_reap(child);
            panic!("parse the pid in {}: {error}", pid_file.display())
        })
}

/// A fault must hold at a boundary it did not declare: the target is still alive after
/// the observation moved one round short (restart) / no request at all (crash).
///
/// The hold is confirmed without a window that can be slept through and without
/// leaving an orphan: the fault thread polls the loopback on its own interval, so the
/// observation is re-read after that interval has certainly passed, and a target that
/// died anyway is killed and reaped before the assertion fails (see
/// [`kill_and_reap`]).
fn assert_bound_holds(child: &mut std::process::Child, message: &str) {
    if !matches!(child.try_wait(), Ok(None)) {
        kill_and_reap(child);
        panic!("the wrapper target died before its boundary: {message}");
    }
    std::thread::sleep(Duration::from_millis(60));
    let held = matches!(child.try_wait(), Ok(None));
    if !held {
        kill_and_reap(child);
    }
    assert!(held, "{message}");
}

/// The wrapper-kill sequence, in this helper process only: write the wrapper, directly
/// exec it, confirm the pid it registered is the exec'd target, kill the group, and reap
/// the dead child. Every step is bounded, and on a failing assertion the child is killed
/// and reaped before the panic leaves this process. Returns the helper's verdict line.
fn run_kill_path_sequence(dir: &Path) -> String {
    let pid_file = dir.join("child.pid");
    let wrapper = dir.join("cli-wrapper.sh");
    write_spawn_wrapper(&wrapper, Path::new("/bin/sleep"), &pid_file)
        .unwrap_or_else(|error| panic!("write the spawn wrapper: {error}"));

    // The wrapper is executable and execs the target, so it registers its own pid
    // before becoming `/bin/sleep`: the pid it writes is the pid that actually sleeps.
    let mut child = spawn_and_confirm_registration(dir, "the kill-path target");

    // The group kill: a negative pid reaches every descendant, with a direct kill as the
    // documented fallback.
    let registered = child.id();
    let killed = unsafe { libc::kill(-(registered as i32), libc::SIGKILL) == 0 }
        || unsafe { libc::kill(registered as i32, libc::SIGKILL) == 0 };
    if !killed {
        // `SIGKILL` not reaching the target means it is still alive, so reap it before
        // the failing assertion panics rather than orphaning a `/bin/sleep`.
        kill_and_reap(&mut child);
    }
    assert!(killed, "SIGKILL did not reach the exec'd target");
    eventually(
        "the killed child to disappear",
        HELPER_STEP_BOUND,
        &mut child,
        // Reaping the way the bounded runner does: an unreaped corpse still answers
        // `kill(pid, 0)`, so death is confirmed by collecting the exit instead.
        |child| matches!(child.try_wait(), Ok(Some(_))),
    );
    let _ = child.wait();
    "arm-sequence-ok:none".to_string()
}
