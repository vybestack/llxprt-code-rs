//! Session-level current-tool projection. The owner's signal descriptor also holds
//! an advisory lifetime lock; process exit releases it, including after SIGKILL.
use super::{name, publish, InFlight, MAX_RECORD_BYTES};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::Path;

const FILENAME: &str = "tool-in-flight.json";

#[derive(Serialize, Deserialize)]
struct CurrentTool {
    owner: String,
    #[serde(flatten)]
    flight: InFlight,
}

fn read(dir: &Path) -> io::Result<Option<CurrentTool>> {
    let file = match crate::safe_file::open_regular_nofollow(&dir.join(FILENAME)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take((MAX_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(io::Error::other("current-tool record exceeds its byte cap"));
    }
    let current: CurrentTool = serde_json::from_slice(&bytes)?;
    if current.owner.is_empty() {
        return Err(io::Error::other("current-tool record has no owner"));
    }
    Ok(Some(current))
}

fn remove(dir: &Path) -> io::Result<()> {
    std::fs::remove_file(dir.join(FILENAME))?;
    std::fs::File::open(dir)?.sync_all()
}

/// Caller holds the session lock. An unlocked signal descriptor proves that the
/// publisher no longer owns a running invocation, without PID reuse or lease races.
/// Do not touch its durable tool/signal evidence, even after reservation reclaim.
pub(super) fn retire_inactive(dir: &Path) -> io::Result<Option<String>> {
    let Some(current) = read(dir)? else {
        return Ok(None);
    };
    let file = crate::safe_file::open_regular_nofollow(&dir.join(name(&current.owner, "signal")))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            remove(dir)?;
            Ok(None)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Some(current.owner)),
        Err(error) => Err(error),
    }
}

pub(super) fn mark(dir: &Path, owner: &str, flight: &InFlight) -> io::Result<()> {
    if retire_inactive(dir)?.is_some_and(|active| active != owner) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "current tool belongs to another active reservation",
        ));
    }
    publish(
        dir,
        FILENAME,
        &serde_json::to_vec(&CurrentTool {
            owner: owner.to_string(),
            flight: flight.clone(),
        })?,
    )
}

pub(super) fn clear_owned(dir: &Path, owner: &str) -> io::Result<()> {
    if read(dir)?.is_some_and(|current| current.owner == owner) {
        remove(dir)?;
    }
    Ok(())
}
