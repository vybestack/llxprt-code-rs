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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bounded wait for a poll-style condition. Every fault thread in the drive polls at a
/// fixed interval, so the tests wait the same way and never hang on a dead path: the
/// probe is retried until it holds or the deadline passes, and the deadline is what
/// makes a dead path fail fast rather than hang.
fn eventually(what: &str, deadline: Duration, mut probe: impl FnMut() -> bool) {
    let start = Instant::now();
    while !probe() {
        if start.elapsed() >= deadline {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// The `Prepared` the guard binds to. Only the config home and session are read on the
/// path under test, so a hand-built one avoids a full `prepare()` drive.
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
        !fs::write(&probe, b"x").is_ok(),
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
#[test]
fn spawn_wrapper_registers_pid_and_kill_path_confirms_death() {
    let dir = tempdir("kill-path");
    let pid_file = dir.join("child.pid");
    let wrapper = dir.join("cli-wrapper.sh");
    write_spawn_wrapper(&wrapper, Path::new("/bin/sleep"), &pid_file).unwrap();

    // The wrapper is executable and execs the target, so it registers its own pid
    // before becoming `/bin/sleep`: the pid it writes is the pid that actually sleeps.
    let mut child = Command::new(&wrapper)
        .arg("30")
        .spawn()
        .expect("spawn wrapper child");
    eventually(
        "the wrapper to register its pid",
        Duration::from_secs(30),
        || pid_file.is_file(),
    );
    assert!(pid_file.is_file(), "the wrapper did not register its pid");
    let registered: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        registered,
        child.id(),
        "the registered pid is not the exec'd target"
    );

    // The group kill: a negative pid reaches every descendant, with a direct kill as the
    // documented fallback.
    let killed = unsafe { libc::kill(-(registered as i32), libc::SIGKILL) == 0 }
        || unsafe { libc::kill(registered as i32, libc::SIGKILL) == 0 };
    assert!(killed, "SIGKILL did not reach the exec'd target");
    eventually(
        "the killed child to disappear",
        Duration::from_secs(30),
        // Reaping the way the bounded runner does: an unreaped corpse still answers
        // `kill(pid, 0)`, so death is confirmed by collecting the exit instead.
        || matches!(child.try_wait(), Ok(Some(_))),
    );
    let _ = child.wait();
    cleanup(&dir);
}

/// `arm_mid_run_fault` arms exactly the kill fault a scenario selected, writes the spawn
/// wrapper, and fires at the declared round boundary: the restart fault waits for the
/// second scripted tool round, so one round short it must hold, and at two it must kill
/// the exec'd target's group and confirm the death.
#[test]
fn arm_mid_run_fault_fires_at_the_declared_round_boundary() {
    let dir = tempdir("arm-fault");
    let observations = Arc::new(Mutex::new(loopback::Observations::default()));
    let scen = fault_scenario(&[faults::RESTART_AFTER_ROUND_2]);
    let armed_fault = crate::context_eval::inject::arm_mid_run_fault(
        &scen,
        Path::new("/bin/sleep"),
        &dir,
        observations.clone(),
    )
    .unwrap()
    .expect("the declared restart fault did not arm");

    // The wrapper is installed where the bounded runner picks the target up from, and
    // the target this test execs through it is a real, short-lived child.
    assert!(dir.join("cli-wrapper.sh").is_file());
    let mut child = Command::new(dir.join("cli-wrapper.sh"))
        .arg("30")
        .spawn()
        .expect("spawn wrapper child");

    // One scripted round is one short of the declared boundary: no kill has happened.
    {
        let mut obs = observations.lock().unwrap_or_else(|p| p.into_inner());
        obs.tool_calls_issued = 1;
    }
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, 0) },
        0,
        "the restart fault fired before its declared round boundary"
    );

    // At the second scripted round the fault kills the exec'd target's group, confirms
    // the death, and reports the trigger it executed.
    {
        let mut obs = observations.lock().unwrap_or_else(|p| p.into_inner());
        obs.tool_calls_issued = 2;
    }
    eventually(
        "the restart fault to kill the target",
        Duration::from_secs(30),
        || matches!(child.try_wait(), Ok(Some(_))),
    );
    let trigger = armed_fault
        .handle
        .join()
        .ok()
        .flatten()
        .expect("the restart fault never executed");
    assert_eq!(
        trigger,
        faults::MidRunFault::Restart.trigger(),
        "the executed trigger is not the declared one"
    );
    let _ = child.wait();

    // A scenario with no kill fault arms nothing: an undeclared fault must not run.
    let none = crate::context_eval::inject::arm_mid_run_fault(
        &fault_scenario(&[]),
        Path::new("/bin/sleep"),
        &dir,
        observations.clone(),
    )
    .unwrap();
    assert!(none.is_none(), "an undeclared fault armed");
    cleanup(&dir);
}

/// The crash fault's boundary is a provider request in flight, so it must not fire on
/// scripted tool rounds alone: an armed crash fault holds through five rounds and fires
/// the moment a request is observed.
#[test]
fn arm_mid_run_fault_crash_boundary_is_a_request_in_flight() {
    let dir = tempdir("arm-crash");
    let observations = Arc::new(Mutex::new(loopback::Observations::default()));
    let scen = fault_scenario(&[faults::CRASH_AT_SEND]);
    let armed_fault = crate::context_eval::inject::arm_mid_run_fault(
        &scen,
        Path::new("/bin/sleep"),
        &dir,
        observations.clone(),
    )
    .unwrap()
    .expect("the declared crash fault did not arm");

    let mut child = Command::new(dir.join("cli-wrapper.sh"))
        .arg("30")
        .spawn()
        .expect("spawn wrapper child");
    {
        let mut obs = observations.lock().unwrap_or_else(|p| p.into_inner());
        obs.tool_calls_issued = 5;
    }
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, 0) },
        0,
        "the crash fault fired without a request in flight"
    );

    {
        let mut obs = observations.lock().unwrap_or_else(|p| p.into_inner());
        obs.requests.push(loopback::ObservedRequest {
            index: 0,
            body_bytes: 42,
            tool_names: Vec::new(),
            streamed: false,
        });
    }
    eventually(
        "the crash fault to kill the target",
        Duration::from_secs(30),
        || matches!(child.try_wait(), Ok(Some(_))),
    );
    let trigger = armed_fault
        .handle
        .join()
        .ok()
        .flatten()
        .expect("the crash fault never executed");
    assert_eq!(trigger, faults::MidRunFault::Crash.trigger());
    let _ = child.wait();
    cleanup(&dir);
}
