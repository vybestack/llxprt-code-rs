#[cfg(test)]
mod tests;

use super::*;

/// The bound (bytes) for a persisted scalar (branch id, parent id, cwd) interpolated
/// into a corruption diagnostic, so one over-sized persisted field can never carry an
/// unbounded error payload. Over-limit bytes are replaced with a stable truncated prefix
/// plus a marker instead of being embedded; the CLI applies a final diagnostic bound on
/// top of every surfaced error.
const MAX_RENDERED_FIELD_BYTES: usize = 256;

/// Render a persisted scalar for a diagnostic, bounded at a UTF-8 boundary so the
/// resulting message stays small; an over-limit value shows a stable truncated prefix
/// plus `...`, never its full bytes.
fn render_field(s: &str) -> String {
    if s.len() <= MAX_RENDERED_FIELD_BYTES {
        return s.to_string();
    }
    let mut end = MAX_RENDERED_FIELD_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out: String = s[..end].to_string();
    out.push_str("...");
    out
}

impl SessionState {
    /// Validate every semantic invariant of a persisted state. Physical
    /// (unreadable/wrong-version) problems are caught before this is called.
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        self.validate_header()?;
        let index = self.branch_index()?;
        for branch in &self.branches {
            self.validate_branch(branch, &index)?;
        }
        self.validate_lineages(&index)?;
        self.validate_cwd()
    }

    fn validate_header(&self) -> Result<(), StoreError> {
        if self.version != STORE_VERSION {
            return Err(StoreError::Corrupt(format!(
                "unsupported state version {}",
                self.version
            )));
        }
        if self.session_id.is_empty() {
            return Err(StoreError::Corrupt("empty session_id".to_string()));
        }
        if self.branches.len() > MAX_BRANCHES {
            return Err(StoreError::Corrupt(format!(
                "too many branches (limit {MAX_BRANCHES})"
            )));
        }
        self.validate_tool_entry_cap()
    }

    fn validate_tool_entry_cap(&self) -> Result<(), StoreError> {
        let mut total = 0usize;
        for branch in &self.branches {
            for round in &branch.rounds {
                total = total.saturating_add(round.calls.len());
                if total > MAX_TOOL_ENTRIES {
                    return Err(StoreError::Corrupt(format!(
                        "too many tool entries (limit {MAX_TOOL_ENTRIES})"
                    )));
                }
            }
        }
        Ok(())
    }

    fn branch_index(&self) -> Result<std::collections::HashMap<&str, usize>, StoreError> {
        let mut index = std::collections::HashMap::with_capacity(self.branches.len());
        for (position, branch) in self.branches.iter().enumerate() {
            if branch.branch_id.is_empty()
                || index.insert(branch.branch_id.as_str(), position).is_some()
            {
                return Err(StoreError::Corrupt(format!(
                    "duplicate or empty branch_id {:?}",
                    render_field(&branch.branch_id)
                )));
            }
        }
        Ok(index)
    }

    fn validate_branch(
        &self,
        branch: &BranchRecord,
        index: &std::collections::HashMap<&str, usize>,
    ) -> Result<(), StoreError> {
        self.validate_branch_shape(branch, index)?;
        self.validate_parent_relation(branch, index)?;
        self.validate_branch_lifecycle(branch)?;
        self.validate_branch_sequence(branch)?;
        self.validate_tool_calls(branch)
    }

    fn validate_branch_shape(
        &self,
        branch: &BranchRecord,
        index: &std::collections::HashMap<&str, usize>,
    ) -> Result<(), StoreError> {
        if branch.turn == 0 {
            return Err(branch_corrupt(branch, "turn must be 1-based"));
        }
        if branch.attempt == 0 {
            return Err(branch_corrupt(branch, "attempt must be 1-based"));
        }
        match branch.parent_branch.as_deref() {
            None if branch.parent_turn != 0 || branch.parent_attempt != 0 => {
                return Err(branch_corrupt(branch, "parent metadata on a root"));
            }
            None if branch.turn != 1 => {
                return Err(branch_corrupt(
                    branch,
                    "a branch without a parent must be at turn 1",
                ));
            }
            Some(_) if branch.turn < 2 => {
                return Err(branch_corrupt(
                    branch,
                    "a turn-1 child is corrupt (a child of a parent cannot be turn 1)",
                ));
            }
            _ => {}
        }
        if let Some(parent) = branch.parent_branch.as_deref() {
            if !index.contains_key(parent) {
                return Err(StoreError::Corrupt(format!(
                    "branch {}: unknown parent {}",
                    render_field(&branch.branch_id),
                    render_field(parent)
                )));
            }
        }
        if crate::limits::prompt_digest(&branch.prompt) != branch.digest {
            return Err(branch_corrupt(branch, "prompt digest mismatch"));
        }
        Ok(())
    }

    fn validate_parent_relation(
        &self,
        branch: &BranchRecord,
        index: &std::collections::HashMap<&str, usize>,
    ) -> Result<(), StoreError> {
        let Some(parent_id) = branch.parent_branch.as_deref() else {
            return Ok(());
        };
        let parent = &self.branches[index[parent_id]];
        if parent.turn != branch.parent_turn || parent.attempt != branch.parent_attempt {
            return Err(branch_corrupt(
                branch,
                "parent metadata turn/attempt disagree with the parent branch",
            ));
        }
        let expected_turn = parent
            .turn
            .checked_add(1)
            .ok_or_else(|| branch_corrupt(branch, "parent turn overflow"))?;
        if branch.turn != expected_turn {
            return Err(branch_corrupt(
                branch,
                "child turn must be the parent's turn + 1",
            ));
        }
        if parent.lifecycle != Lifecycle::Completed {
            return Err(branch_corrupt(branch, "parent branch is not completed"));
        }
        Ok(())
    }

    fn validate_branch_lifecycle(&self, branch: &BranchRecord) -> Result<(), StoreError> {
        if branch.prompt.len() > MAX_PROMPT_BYTES {
            return Err(branch_corrupt(branch, "prompt exceeds its byte cap"));
        }
        if branch.owner.len() > MAX_RENDERED_FIELD_BYTES {
            return Err(branch_corrupt(branch, "owner token exceeds its byte cap"));
        }
        match branch.lifecycle {
            Lifecycle::Pending => {
                if branch.reserved_at == 0 || branch.lease_expiry <= branch.reserved_at {
                    return Err(branch_corrupt(branch, "invalid reservation lease fields"));
                }
                if branch.owner.is_empty() || !branch.summary.is_empty() || !branch.error.is_empty()
                {
                    return Err(branch_corrupt(
                        branch,
                        "pending lifecycle fields are inconsistent",
                    ));
                }
            }
            Lifecycle::Completed => {
                if branch.reserved_at != 0 || branch.lease_expiry != 0 {
                    return Err(branch_corrupt(
                        branch,
                        "terminal branch retains reservation lease fields",
                    ));
                }
                if !branch.error.is_empty() {
                    return Err(branch_corrupt(
                        branch,
                        "completed lifecycle fields are inconsistent",
                    ));
                }
                let Some(final_round) = branch.rounds.last() else {
                    return Err(branch_corrupt(
                        branch,
                        "completed branch has no final round",
                    ));
                };
                if !final_round.calls.is_empty() || branch.summary != final_round.assistant {
                    return Err(branch_corrupt(
                        branch,
                        "completed summary disagrees with the final no-tool round",
                    ));
                }
            }
            Lifecycle::Failed => {
                if branch.reserved_at != 0 || branch.lease_expiry != 0 {
                    return Err(branch_corrupt(
                        branch,
                        "terminal branch retains reservation lease fields",
                    ));
                }
                if branch.error.is_empty() || !branch.summary.is_empty() {
                    return Err(branch_corrupt(
                        branch,
                        "failed lifecycle fields are inconsistent",
                    ));
                }
            }
        }
        if branch.summary.len() > crate::limits::MAX_TURN_ASSISTANT_BYTES {
            return Err(branch_corrupt(branch, "summary exceeds its byte cap"));
        }
        if branch.error.len() > crate::redact::MAX_ERROR_TEXT_BYTES {
            return Err(branch_corrupt(branch, "error exceeds its byte cap"));
        }
        Ok(())
    }

    fn validate_branch_sequence(&self, branch: &BranchRecord) -> Result<(), StoreError> {
        let sequence = branch
            .branch_id
            .strip_prefix('b')
            .and_then(|digits| digits.parse::<u64>().ok());
        match sequence {
            Some(number) if number >= 1 && number <= self.next_branch_seq => {}
            _ => {
                return Err(StoreError::Corrupt(format!(
                    "branch {}: branch id is outside the allocated sequence (next_branch_seq {})",
                    render_field(&branch.branch_id),
                    self.next_branch_seq
                )));
            }
        }
        Ok(())
    }

    fn validate_tool_calls(&self, branch: &BranchRecord) -> Result<(), StoreError> {
        // No round-count ceiling: round budgets are declared per run (`maxTurnsPerPrompt`
        // is unlimited unless capped), so a long branch is policy, not corruption. The
        // byte totals below remain the corruption signal, as they are for tool calls.
        let mut ids = std::collections::HashSet::new();
        let mut calls = 0usize;
        let mut assistant_bytes = 0usize;
        let mut argument_bytes = 0usize;
        let mut result_bytes = 0usize;
        for (position, round) in branch.rounds.iter().enumerate() {
            let is_completed_final =
                branch.lifecycle == Lifecycle::Completed && position + 1 == branch.rounds.len();
            if round.calls.is_empty() && !is_completed_final {
                return Err(branch_corrupt(
                    branch,
                    "only a completed branch may have a final no-tool round",
                ));
            }
            assistant_bytes = checked_total(branch, assistant_bytes, round.assistant.len())?;
            let mut mapped_bytes = round.assistant.len();
            self.validate_mapped_response_size(branch, mapped_bytes)?;
            for call in &round.calls {
                self.validate_persisted_call(branch, call, &mut ids)?;
                if !call.refused {
                    calls = checked_total(branch, calls, 1)?;
                }
                argument_bytes = checked_total(branch, argument_bytes, call.args.len())?;
                result_bytes = checked_total(branch, result_bytes, call.result.len())?;
                mapped_bytes = mapped_bytes
                    .checked_add(call.id.len())
                    .and_then(|total| total.checked_add(call.name.len()))
                    .and_then(|total| total.checked_add(call.args.len()))
                    .ok_or_else(|| branch_corrupt(branch, "mapped response size overflow"))?;
                self.validate_mapped_response_size(branch, mapped_bytes)?;
            }
        }
        if assistant_bytes > crate::limits::MAX_TURN_ASSISTANT_BYTES {
            return Err(branch_corrupt(
                branch,
                "assistant transcript exceeds its byte cap",
            ));
        }
        if argument_bytes > crate::limits::MAX_TURN_ARGS_BYTES {
            return Err(branch_corrupt(
                branch,
                "tool arguments exceed their byte cap",
            ));
        }
        if result_bytes > crate::limits::MAX_TURN_OUTPUT_BYTES {
            return Err(branch_corrupt(branch, "tool results exceed their byte cap"));
        }
        Ok(())
    }

    fn validate_mapped_response_size(
        &self,
        branch: &BranchRecord,
        mapped_bytes: usize,
    ) -> Result<(), StoreError> {
        if mapped_bytes > crate::limits::MAX_RESPONSE_BYTES {
            Err(branch_corrupt(
                branch,
                "mapped response exceeds the model response byte cap",
            ))
        } else {
            Ok(())
        }
    }

    fn validate_persisted_call<'a>(
        &self,
        branch: &BranchRecord,
        call: &'a ToolCallRecord,
        ids: &mut std::collections::HashSet<&'a str>,
    ) -> Result<(), StoreError> {
        if call.id.is_empty() || !ids.insert(call.id.as_str()) {
            return Err(branch_corrupt(branch, "empty or duplicate tool call id"));
        }
        if call.id.len() > crate::limits::MAX_TOOL_CALL_ID_BYTES {
            return Err(branch_corrupt(branch, "tool call id exceeds its byte cap"));
        }
        if call.name.len() > crate::limits::MAX_TOOL_NAME_BYTES {
            return Err(branch_corrupt(branch, "tool name exceeds its byte cap"));
        }
        if !crate::tools::is_known_tool_name(&call.name) {
            return Err(branch_corrupt(branch, "unknown tool name"));
        }
        let object = serde_json::from_str::<serde_json::Value>(&call.args)
            .map(|value| value.is_object())
            .unwrap_or(false);
        if !object {
            return Err(branch_corrupt(
                branch,
                "tool call args are not a JSON object",
            ));
        }
        Ok(())
    }

    fn validate_lineages(
        &self,
        index: &std::collections::HashMap<&str, usize>,
    ) -> Result<(), StoreError> {
        let parents: Vec<Option<usize>> = self
            .branches
            .iter()
            .map(|branch| {
                branch
                    .parent_branch
                    .as_deref()
                    .and_then(|parent| index.get(parent).copied())
            })
            .collect();
        let mut color = vec![0u8; self.branches.len()];
        let mut path = Vec::with_capacity(64);
        for start in 0..self.branches.len() {
            if color[start] == 2 {
                continue;
            }
            self.validate_lineage(start, &parents, &mut color, &mut path)?;
        }
        Ok(())
    }

    fn validate_lineage(
        &self,
        start: usize,
        parents: &[Option<usize>],
        color: &mut [u8],
        path: &mut Vec<usize>,
    ) -> Result<(), StoreError> {
        let mut current = start;
        path.clear();
        loop {
            match color[current] {
                1 => {
                    return Err(branch_corrupt(
                        &self.branches[current],
                        "cycle among parent links",
                    ));
                }
                2 => break,
                _ => {
                    color[current] = 1;
                    path.push(current);
                    match parents[current] {
                        Some(parent) => current = parent,
                        None => break,
                    }
                }
            }
        }
        let mut keys = std::collections::HashSet::with_capacity(path.len());
        for &node in path.iter() {
            keys.insert((self.branches[node].turn, self.branches[node].attempt));
            color[node] = 2;
        }
        if keys.len() != path.len() {
            return Err(branch_corrupt(
                &self.branches[path[0]],
                "duplicate (turn,attempt) identity in lineage",
            ));
        }
        Ok(())
    }

    fn validate_cwd(&self) -> Result<(), StoreError> {
        match &self.cwd {
            Some(cwd) if cwd.is_empty() || !Path::new(cwd).is_absolute() => {
                Err(StoreError::Corrupt(format!(
                    "session cwd {:?} is not an absolute path",
                    render_field(cwd)
                )))
            }
            Some(_) if self.cwd_dev == 0 || self.cwd_ino == 0 => Err(StoreError::Corrupt(
                "session cwd has no filesystem identity".to_string(),
            )),
            None if self.cwd_dev != 0 || self.cwd_ino != 0 => Err(StoreError::Corrupt(
                "session filesystem identity has no cwd".to_string(),
            )),
            _ => Ok(()),
        }
    }
}

fn branch_corrupt(branch: &BranchRecord, message: &str) -> StoreError {
    StoreError::Corrupt(format!(
        "branch {}: {message}",
        render_field(&branch.branch_id)
    ))
}

fn checked_total(branch: &BranchRecord, total: usize, amount: usize) -> Result<usize, StoreError> {
    total
        .checked_add(amount)
        .ok_or_else(|| branch_corrupt(branch, "transcript byte count overflow"))
}
