//! Only supervises the exact no-shell, scripted-backend recovery child.
use super::*;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

pub(super) fn run(mut command: Command, case: &str, round: usize) -> Output {
    // File-backed capture cannot fill a pipe while the parent polls. No descendant
    // processes are launched by the selected child cases (read_file only).
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();
    command.stdout(Stdio::from(stdout.try_clone().unwrap()));
    command.stderr(Stdio::from(stderr.try_clone().unwrap()));
    let mut child = command.spawn().expect("spawn recovery child");
    eprintln!(
        "started case={case} round={round} pid={} deadline=60s",
        child.id()
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => {
                // This Child still owns an unreaped direct child, never a PID
                // obtained from an old log or a process-name search.
                let killed = child.kill();
                let reaped = child.wait();
                panic!("case={case} round={round} deadline/wait failure {other:?}; kill={killed:?} reap={reaped:?}\n{}\n{}", String::from_utf8_lossy(&read(stdout)), String::from_utf8_lossy(&read(stderr)));
            }
        }
    };
    eprintln!(
        "reaped case={case} round={round} pid={} status={status}",
        child.id()
    );
    Output {
        status,
        stdout: read(stdout),
        stderr: read(stderr),
    }
}

fn read(mut file: std::fs::File) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

pub(super) fn assert_executed(output: &std::process::Output, case: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "running 1 test")
            .count(),
        1
    );
    let summaries: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect();
    assert_eq!(summaries.len(), 1, "{stdout}");
    assert!(
        summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "),
        "{stdout}"
    );
    let marker = format!("completed delegated case={case}");
    assert_eq!(
        stderr.lines().filter(|line| *line == marker).count(),
        1,
        "{stderr}"
    );
}
