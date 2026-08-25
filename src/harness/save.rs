use super::*;

/// Persist the exact subprocess output for one turn durably: the raw stdout bytes (verbatim,
pub(super) fn ensure_artifact_subdir(
    root: &openat::Dir,
    name: &str,
) -> Result<openat::Dir, String> {
    match root.sub_dir(name) {
        Ok(dir) => return Ok(dir),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("open artifact directory: {error}"));
        }
        Err(_) => {}
    }
    match root.create_dir(name, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("create artifact directory: {error}")),
    }
    root.sub_dir(name)
        .map_err(|error| format!("open artifact directory: {error}"))
}

/// untrimmed, possibly not valid UTF-8) as `<scenario>/<session>.turn<N>.json`, the raw stderr
/// bytes as `<scenario>/<session>.turn<N>.stderr`, and typed metadata as
/// `<scenario>/<session>.turn<N>.meta.json`.
///
/// The output root and scenario directory are retained descriptors. Every artifact is staged
/// descriptor-relatively into a private 0600 candidate, written, and synced. Installation is
/// bound to the candidate descriptor and refuses an existing final name: Linux links an unnamed
/// inode, while macOS clones the retained descriptor. Installed bytes are then checked against
/// the staged digest. The scenario directory is synced before the completion marker is installed
/// by the same process and synced again. A failure never removes a pathname that an adversary
/// could have replaced; without the final marker, a partial set is incomplete evidence. The raw
/// subprocess bytes remain verbatim and are never decoded for publication.
pub fn save_turn(
    out_root: &Path,
    scenario: &str,
    session: &str,
    turn: u32,
    result: &BbResult,
) -> Result<(), String> {
    if !crate::session::is_safe_component(scenario) || !crate::session::is_safe_component(session) {
        return Err("scenario and session must be safe path components".to_string());
    }
    fs::create_dir_all(out_root).map_err(|error| error.to_string())?;
    let root = crate::tools::open_root(out_root)?;
    let scenario_dir = ensure_artifact_subdir(&root, scenario)?;
    let base = format!("{session}.turn{turn}");
    let stages = turn_stages(&base, session, turn, result)?;

    let mut candidates = Vec::with_capacity(stages.len());
    for stage in &stages {
        candidates.push(stage_at(&scenario_dir, &stage.name, &stage.bytes)?);
    }
    for (stage, candidate) in stages.iter().zip(candidates.iter()) {
        publish_stage_at(&scenario_dir, candidate, &stage.name)?;
    }
    sync_artifact_dir(&scenario_dir)
        .map_err(|error| format!("sync artifact directory: {error}"))?;
    for (stage, candidate) in stages.iter().zip(candidates.iter()) {
        verify_candidate_at(&scenario_dir, candidate, &stage.name)
            .map_err(|error| format!("verify durable {}: {error}", stage.name))?;
    }

    let marker_name = format!("{base}.done");
    let marker = stage_at(&scenario_dir, &marker_name, b"done\n")?;
    publish_stage_at(&scenario_dir, &marker, &marker_name)?;
    sync_artifact_dir(&scenario_dir).map_err(|error| {
        format!("completion marker installed but directory durability is unconfirmed: {error}")
    })?;
    for (stage, candidate) in stages.iter().zip(candidates.iter()) {
        verify_candidate_at(&scenario_dir, candidate, &stage.name)
            .map_err(|error| format!("verify completed {}: {error}", stage.name))?;
    }
    verify_candidate_at(&scenario_dir, &marker, &marker_name)
        .map_err(|error| format!("verify completion marker: {error}"))
}

fn turn_stages(
    base: &str,
    session: &str,
    turn: u32,
    result: &BbResult,
) -> Result<[StagedFile; 3], String> {
    let meta = serde_json::json!({
        "session": session,
        "turn": turn,
        "ok": result.ok,
        "status": result.status,
        "exit": result.exit,
        "session_id": result.session_id,
        "attempt": result.attempt,
        "branch_id": result.branch_id,
        "branch": result.branch,
        "replayed": result.replayed,
        "tool_calls": result.tool_calls,
        "prompt_digest": result.prompt_digest,
        "summary": result.summary,
        "stdout_truncated": result.stdout_truncated,
        "stderr_truncated": result.stderr_truncated,
        "combined_truncated": result.combined_truncated,
        "error": {
            "code": result.error_code,
            "message": result.error_message,
        },
    });
    let meta = serde_json::to_vec_pretty(&meta).map_err(|error| error.to_string())?;
    Ok([
        StagedFile {
            name: format!("{base}.json"),
            bytes: result.raw_stdout.clone(),
        },
        StagedFile {
            name: format!("{base}.stderr"),
            bytes: result.stderr.clone(),
        },
        StagedFile {
            name: format!("{base}.meta.json"),
            bytes: meta,
        },
    ])
}

struct StagedFile {
    name: String,
    bytes: Vec<u8>,
}
