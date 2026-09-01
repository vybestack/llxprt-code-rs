use super::*;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

#[test]
fn sink_is_create_only_private_and_writes_complete_terminal_line() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.jsonl");
    let profiler = Profiler::initialize(&path).unwrap();
    profiler
        .event("profile_parsed", EventData::default())
        .unwrap();
    profiler.event("pre_exit", EventData::default()).unwrap();
    let summary = profiler.finalize("ok").unwrap();
    assert_eq!(summary.path, path);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let lines: Vec<_> = bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines.len(), 4);
    let events: Vec<serde_json::Value> = lines
        .into_iter()
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(events[0]["phase"], "startup_observed");
    assert_eq!(events[3]["phase"], "profile_complete");
    assert_eq!(events[3]["outcome"], "ok");
    assert_eq!(events[1]["observed_after"], "startup_observed");
    assert!(Profiler::initialize(&path).is_err());
}

#[test]
fn unsafe_destination_leaves_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(Profiler::initialize(&directory).is_err());

    let target = temp.path().join("target");
    std::fs::write(&target, "x").unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(Profiler::initialize(&link).is_err());

    let fifo = temp.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(Profiler::initialize(&fifo).is_err());
}

#[test]
fn profile_file_stays_bound_to_original_inode() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.jsonl");
    let profiler = Profiler::initialize(&path).unwrap();
    let original = std::fs::metadata(&path).unwrap().ino();
    let moved = temp.path().join("moved");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, "replacement").unwrap();
    profiler.event("pre_exit", EventData::default()).unwrap();
    profiler.finalize("ok").unwrap();
    assert_eq!(std::fs::metadata(&moved).unwrap().ino(), original);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
    assert!(std::fs::read_to_string(&moved)
        .unwrap()
        .contains("profile_complete"));
}
