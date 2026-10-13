//! Semantic validation and rollback-safe application of log transactions.

use super::*;
use log::{Event, EventBatch};

pub(super) fn apply_batch(state: &mut SessionState, batch: &EventBatch) -> Result<(), StoreError> {
    if batch.events.len() != 1 {
        return Err(corrupt(
            "a transaction must contain exactly one logical event",
        ));
    }
    let mut candidate = state.clone();
    apply_event(&mut candidate, &batch.events[0])?;
    candidate.validate()?;
    *state = candidate;
    Ok(())
}

fn apply_event(state: &mut SessionState, event: &Event) -> Result<(), StoreError> {
    match event {
        Event::Provider {
            branch_id,
            owner,
            command,
        } => provider_event(state, branch_id, owner, command),
        Event::RenderContract {
            branch_id,
            owner,
            publication,
        } => publish_render(state, branch_id, owner, publication),
        Event::Telemetry {
            branch_id,
            owner,
            telemetry,
        } => {
            let branch = state
                .branches
                .iter_mut()
                .find(|b| &b.branch_id == branch_id)
                .ok_or_else(|| corrupt("telemetry names unknown branch"))?;
            if &branch.owner != owner || !telemetry.valid() {
                return Err(corrupt("invalid telemetry owner or payload"));
            }
            branch.telemetry = Some(*telemetry.clone());
            Ok(())
        }
        Event::BranchReserved {
            cwd,
            cwd_dev,
            cwd_ino,
            branch,
            next_branch_seq,
        } => reserve(state, cwd, *cwd_dev, *cwd_ino, branch, *next_branch_seq),
        Event::BranchReclaimed {
            branch_id,
            prompt,
            owner,
            reserved_at,
            lease_expiry,
        } => reclaim(state, branch_id, prompt, owner, *reserved_at, *lease_expiry),
        Event::StreamRetry {
            branch_id,
            owner,
            record,
        } => stream_retry(state, branch_id, owner, record),
        Event::LeaseRenewed {
            branch_id,
            owner,
            lease_expiry,
        } => renew(state, branch_id, owner, *lease_expiry),
        Event::Checkpoint {
            branch_id,
            owner,
            rounds,
            lease_expiry,
        } => checkpoint(state, branch_id, owner, rounds, *lease_expiry),
        Event::BranchCompleted {
            branch_id,
            owner,
            rounds,
            summary,
        } => complete(state, branch_id, owner, rounds, summary),
        Event::BranchFailed {
            branch_id,
            owner,
            rounds,
            error,
        } => fail(state, branch_id, owner, rounds, error),
    }
}

fn reserve(
    state: &mut SessionState,
    cwd: &Option<String>,
    cwd_dev: u64,
    cwd_ino: u64,
    branch: &BranchRecord,
    next_branch_seq: u64,
) -> Result<(), StoreError> {
    if state
        .branches
        .iter()
        .any(|item| item.branch_id == branch.branch_id)
        || branch.lifecycle != Lifecycle::Pending
        || next_branch_seq
            != state
                .next_branch_seq
                .checked_add(1)
                .ok_or_else(|| corrupt("branch sequence overflow"))?
    {
        return Err(corrupt("branch reservation has an illegal source state"));
    }
    match (&state.cwd, cwd) {
        (None, Some(path)) if cwd_dev != 0 && cwd_ino != 0 => {
            state.cwd = Some(path.clone());
            state.cwd_dev = cwd_dev;
            state.cwd_ino = cwd_ino;
        }
        (Some(current), Some(path))
            if current == path && state.cwd_dev == cwd_dev && state.cwd_ino == cwd_ino => {}
        _ => return Err(corrupt("branch reservation has inconsistent cwd pinning")),
    }
    state.next_branch_seq = next_branch_seq;
    state.provider_epoch = state
        .provider_epoch
        .checked_add(1)
        .ok_or_else(|| corrupt("provider epoch overflow"))?;
    state.branches.push(branch.clone());
    Ok(())
}

fn pending<'a>(
    state: &'a mut SessionState,
    branch_id: &str,
    owner: &str,
) -> Result<&'a mut BranchRecord, StoreError> {
    let branch = state
        .branches
        .iter_mut()
        .find(|branch| branch.branch_id == branch_id)
        .ok_or_else(|| corrupt("transaction names an unknown branch"))?;
    if branch.lifecycle != Lifecycle::Pending || branch.owner != owner {
        return Err(corrupt("transaction has an illegal lifecycle or owner"));
    }
    Ok(branch)
}

fn reclaim(
    state: &mut SessionState,
    branch_id: &str,
    prompt: &str,
    owner: &str,
    reserved_at: u64,
    lease_expiry: u64,
) -> Result<(), StoreError> {
    state.provider_epoch = state
        .provider_epoch
        .checked_add(1)
        .ok_or_else(|| corrupt("provider epoch overflow"))?;
    let branch = state
        .branches
        .iter_mut()
        .find(|branch| branch.branch_id == branch_id)
        .ok_or_else(|| corrupt("reclaim names an unknown branch"))?;
    if branch.lifecycle != Lifecycle::Pending
        || branch.lease_expiry > reserved_at
        || owner.is_empty()
        || lease_expiry <= reserved_at
    {
        return Err(corrupt("branch reclaim has an illegal source state"));
    }
    branch.prompt = prompt.to_string();
    branch.digest = crate::limits::prompt_digest(prompt);
    branch.owner = owner.to_string();
    branch.reserved_at = reserved_at;
    branch.lease_expiry = lease_expiry;
    branch.telemetry = None;
    // Committed completions survive lease acquisition.

    branch.stream_retries.clear();
    branch.summary.clear();
    branch.error.clear();
    Ok(())
}

fn renew(
    state: &mut SessionState,
    branch_id: &str,
    owner: &str,
    lease_expiry: u64,
) -> Result<(), StoreError> {
    let branch = pending(state, branch_id, owner)?;
    if lease_expiry <= branch.reserved_at {
        return Err(corrupt("lease renewal has an invalid expiry"));
    }
    branch.lease_expiry = lease_expiry;
    Ok(())
}

fn append_rounds(branch: &mut BranchRecord, rounds: &[RoundRecord]) -> Result<(), StoreError> {
    if rounds.is_empty() {
        return Ok(());
    }
    branch.rounds.extend_from_slice(rounds);
    Ok(())
}

fn checkpoint(
    state: &mut SessionState,
    branch_id: &str,
    owner: &str,
    rounds: &[RoundRecord],
    lease_expiry: u64,
) -> Result<(), StoreError> {
    let branch = pending(state, branch_id, owner)?;
    append_rounds(branch, rounds)?;
    if lease_expiry <= branch.reserved_at {
        return Err(corrupt("checkpoint has an invalid lease expiry"));
    }
    branch.lease_expiry = lease_expiry;
    Ok(())
}

fn complete(
    state: &mut SessionState,
    branch_id: &str,
    owner: &str,
    rounds: &[RoundRecord],
    summary: &str,
) -> Result<(), StoreError> {
    let branch = pending(state, branch_id, owner)?;
    append_rounds(branch, rounds)?;
    branch.summary = summary.to_string();
    branch.lifecycle = Lifecycle::Completed;
    clear_lease(branch);
    Ok(())
}

fn fail(
    state: &mut SessionState,
    branch_id: &str,
    owner: &str,
    rounds: &[RoundRecord],
    error: &str,
) -> Result<(), StoreError> {
    let branch = pending(state, branch_id, owner)?;
    append_rounds(branch, rounds)?;
    branch.error = error.to_string();
    branch.lifecycle = Lifecycle::Failed;
    clear_lease(branch);
    Ok(())
}

fn clear_lease(branch: &mut BranchRecord) {
    branch.reserved_at = 0;
    branch.lease_expiry = 0;
}

fn corrupt(message: &str) -> StoreError {
    StoreError::Corrupt(message.to_string())
}

pub(super) fn suffix<'a>(
    persisted: &[RoundRecord],
    supplied: &'a [RoundRecord],
) -> Result<&'a [RoundRecord], StoreError> {
    if persisted.len() > supplied.len() {
        return Err(StoreError::Invalid(
            "persisted rounds are not a prefix of supplied rounds".into(),
        ));
    }
    let persisted_bytes = serde_json::to_vec(persisted)
        .map_err(|error| StoreError::Corrupt(format!("serialize rounds: {error}")))?;
    let supplied_prefix = serde_json::to_vec(&supplied[..persisted.len()])
        .map_err(|error| StoreError::Corrupt(format!("serialize rounds: {error}")))?;
    if persisted_bytes != supplied_prefix {
        return Err(StoreError::Invalid(
            "persisted rounds are not a prefix of supplied rounds".into(),
        ));
    }
    Ok(&supplied[persisted.len()..])
}

pub(super) fn rounds_equal(
    left: &[RoundRecord],
    right: &[RoundRecord],
) -> Result<bool, StoreError> {
    let left = serde_json::to_vec(left)
        .map_err(|error| StoreError::Corrupt(format!("serialize rounds: {error}")))?;
    let right = serde_json::to_vec(right)
        .map_err(|error| StoreError::Corrupt(format!("serialize rounds: {error}")))?;
    Ok(left == right)
}

fn publish_render(
    state: &mut SessionState,
    branch: &str,
    owner: &str,
    publication: &crate::context_gateway::RenderPublication,
) -> Result<(), StoreError> {
    pending(state, branch, owner)?;
    if state.render_contracts.last() != Some(publication) {
        state.render_contracts.push(publication.clone());
    }
    Ok(())
}

fn stream_retry(
    state: &mut SessionState,
    branch: &str,
    owner: &str,
    record: &StreamRetryRecord,
) -> Result<(), StoreError> {
    let branch = pending(state, branch, owner)?;
    super::stream_retry::validate(record)?;
    branch.stream_retries.push(record.clone());
    Ok(())
}

fn provider_event(
    state: &mut SessionState,
    branch: &str,
    owner: &str,
    command: &crate::context_gateway::Command,
) -> Result<(), StoreError> {
    use crate::context_gateway::Command;
    pending(state, branch, owner)?;
    match command {
        Command::Intent(intent)
            if intent.branch == branch
                && intent.owner == owner
                && intent.epoch == state.provider_epoch => {}
        Command::Recover { branch: target } if target == branch => {}
        Command::Stage { identity, .. }
        | Command::Admit { identity, .. }
        | Command::Reject { identity }
        | Command::Complete { identity, .. } => {
            if !state.provider_turns.iter().any(|r| {
                r.intent.identity == *identity
                    && r.intent.branch == branch
                    && r.intent.owner == owner
            }) {
                return Err(corrupt("provider command crosses owner or branch fence"));
            }
        }
        _ => return Err(corrupt("provider intent has invalid owner or epoch")),
    }
    crate::context_gateway::apply(&mut state.provider_turns, command)
        .map_err(|e| corrupt(&e.to_string()))
}
