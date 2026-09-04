use super::*;

struct ParentRef {
    branch_id: Option<String>,
    turn: u32,
    attempt: u32,
}

struct NewBranch<'a> {
    target: u32,
    parent: ParentRef,
    prompt: &'a str,
    lease: &'a RequestLease,
}

struct ReservationInput<'a> {
    branch_id: &'a str,
    turn: u32,
    attempt: u32,
    retry: bool,
    prompt: &'a str,
    prior: Vec<HistoryTurn>,
    owner: String,
}

impl SessionStore {
    /// Decide and persist the next action for a request, all under the exclusive lock.
    /// `cwd` is pinned atomically: set once on the first turn, verified after.
    pub fn start_request(
        &self,
        requested_turn: Option<u32>,
        branch: Option<&str>,
        prompt: &str,
        cwd: &Path,
    ) -> Result<ReservedRequest, StoreError> {
        let workspace = crate::tools::WorkspaceCap::open(cwd).map_err(StoreError::Invalid)?;
        self.start_request_with_workspace(requested_turn, branch, prompt, cwd, &workspace)
    }

    /// Reserve against the exact workspace descriptor opened before agent construction.
    pub fn start_request_with_workspace(
        &self,
        requested_turn: Option<u32>,
        branch: Option<&str>,
        prompt: &str,
        cwd: &Path,
        workspace: &crate::tools::WorkspaceCap,
    ) -> Result<ReservedRequest, StoreError> {
        let canonical = cwd
            .canonicalize()
            .map_err(|error| StoreError::Invalid(format!("resolve workspace: {error}")))?;
        let current = crate::tools::WorkspaceCap::open(&canonical).map_err(StoreError::Invalid)?;
        if current.identity() != workspace.identity() {
            return Err(StoreError::Invalid(
                "workspace identity changed before reservation".to_string(),
            ));
        }
        self.locked(|| {
            self.start_request_locked(
                requested_turn,
                branch,
                prompt,
                &canonical,
                workspace.identity(),
            )
        })
    }

    fn start_request_locked(
        &self,
        requested_turn: Option<u32>,
        branch: Option<&str>,
        prompt: &str,
        cwd: &Path,
        identity: (u64, u64),
    ) -> Result<ReservedRequest, StoreError> {
        let mut state = self.prepare_request_state(prompt, cwd, identity)?;
        let current = Self::select_current(&state, branch)?;
        let target = Self::request_target(&state, current, requested_turn)?;
        let now = now_secs();
        let lease = RequestLease {
            owner: new_owner(),
            now,
            lease_end: now.saturating_add(LEASE_SECONDS),
        };
        if let Some(existing) = Self::find_existing(&state, current, target, prompt) {
            return self.reserve_existing(&mut state, current, existing, target, prompt, &lease);
        }
        self.reserve_fresh(&mut state, current, requested_turn, target, prompt, &lease)
    }

    fn prepare_request_state(
        &self,
        prompt: &str,
        cwd: &Path,
        identity: (u64, u64),
    ) -> Result<SessionState, StoreError> {
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(StoreError::Invalid(format!(
                "prompt exceeds the {MAX_PROMPT_BYTES} byte limit"
            )));
        }
        let mut state = self.read()?;
        if state.session_id != self.session_id {
            return Err(StoreError::Corrupt(format!(
                "state session_id {:?} does not match {:?}",
                state.session_id, self.session_id
            )));
        }
        let canonical = cwd.to_string_lossy().to_string();
        match &state.cwd {
            None => {
                state.cwd = Some(canonical);
                state.cwd_dev = identity.0;
                state.cwd_ino = identity.1;
            }
            Some(pinned) if *pinned != canonical => {
                return Err(StoreError::Invalid(format!(
                    "session is pinned to {pinned}; this run used {canonical}"
                )));
            }
            Some(_) if (state.cwd_dev, state.cwd_ino) != identity => {
                return Err(StoreError::Invalid(
                    "session workspace identity changed".to_string(),
                ));
            }
            Some(_) => {}
        }
        Ok(state)
    }

    fn request_target(
        state: &SessionState,
        current: Option<usize>,
        requested: Option<u32>,
    ) -> Result<u32, StoreError> {
        if let Some(turn) = requested {
            Self::validate_requested_turn(state, current, turn)?;
            return Ok(turn);
        }
        match current {
            None => Ok(1),
            Some(index) => state.branches[index]
                .turn
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("turn overflow".to_string())),
        }
    }

    fn validate_requested_turn(
        state: &SessionState,
        current: Option<usize>,
        requested: u32,
    ) -> Result<(), StoreError> {
        if requested == 0 {
            return Err(StoreError::Invalid("turn numbers are 1-based".to_string()));
        }
        let Some(index) = current else {
            return Ok(());
        };
        let latest = Self::lineage_latest(&state.branches, index);
        let max_allowed = latest
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("turn overflow".to_string()))?;
        if requested > max_allowed {
            return Err(StoreError::Invalid(format!(
                "requested turn {requested} is beyond the selected lineage's latest turn {latest} + 1"
            )));
        }
        Ok(())
    }

    fn reserve_existing(
        &self,
        state: &mut SessionState,
        current: Option<usize>,
        existing: usize,
        target: u32,
        prompt: &str,
        lease: &RequestLease,
    ) -> Result<ReservedRequest, StoreError> {
        match state.branches[existing].lifecycle {
            Lifecycle::Completed => Ok(Self::replay_reservation(
                &state.branches[existing],
                lease.owner.clone(),
            )),
            Lifecycle::Failed => {
                self.reserve_retry(state, current, existing, target, prompt, lease)
            }
            Lifecycle::Pending => self.reclaim_pending(state, current, existing, prompt, lease),
        }
    }

    fn replay_reservation(record: &BranchRecord, owner: String) -> ReservedRequest {
        ReservedRequest {
            branch_id: record.branch_id.clone(),
            turn: record.turn,
            attempt: record.attempt,
            replay: true,
            retry: false,
            rounds: record.rounds.clone(),
            summary: record.summary.clone(),
            prompt: record.prompt.clone(),
            history: Vec::new(),
            owner,
        }
    }

    fn reserve_retry(
        &self,
        state: &mut SessionState,
        current: Option<usize>,
        existing: usize,
        target: u32,
        prompt: &str,
        lease: &RequestLease,
    ) -> Result<ReservedRequest, StoreError> {
        let prior = self.prior_history(state, current, target);
        let lineage = Self::lineage(state, existing);
        let (parent_branch, parent_turn, parent_attempt) =
            Self::predecessor_parent(&state.branches, &lineage, target);
        let branch_id = self.push_new_branch(
            state,
            NewBranch {
                target,
                parent: ParentRef {
                    branch_id: parent_branch,
                    turn: parent_turn,
                    attempt: parent_attempt,
                },
                prompt,
                lease,
            },
        )?;
        Ok(self.make_retry_reservation(
            &branch_id,
            state,
            target,
            prompt,
            prior,
            lease.owner.clone(),
        ))
    }

    fn reclaim_pending(
        &self,
        state: &mut SessionState,
        current: Option<usize>,
        existing: usize,
        prompt: &str,
        lease: &RequestLease,
    ) -> Result<ReservedRequest, StoreError> {
        let record = &state.branches[existing];
        if record.lease_expiry > lease.now {
            return Err(StoreError::Busy(format!(
                "branch {} is pending for another process",
                record.branch_id
            )));
        }
        let branch_id = record.branch_id.clone();
        let turn = record.turn;
        let attempt = record.attempt;
        let prior = self.prior_history(state, current, turn);
        self.append_event(log::Event::BranchReclaimed {
            branch_id: branch_id.clone(),
            prompt: prompt.to_string(),
            owner: lease.owner.clone(),
            reserved_at: lease.now,
            lease_expiry: lease.lease_end,
        })?;
        Ok(ReservedRequest {
            branch_id,
            turn,
            attempt,
            replay: false,
            retry: false,
            rounds: Vec::new(),
            summary: String::new(),
            prompt: prompt.to_string(),
            history: prior,
            owner: lease.owner.clone(),
        })
    }

    fn reserve_fresh(
        &self,
        state: &mut SessionState,
        current: Option<usize>,
        requested: Option<u32>,
        target: u32,
        prompt: &str,
        lease: &RequestLease,
    ) -> Result<ReservedRequest, StoreError> {
        let prior = self.prior_history(state, current, target);
        let (parent_branch, parent_turn, parent_attempt) =
            Self::fresh_parent(state, current, requested, target);
        let branch_id = self.push_new_branch(
            state,
            NewBranch {
                target,
                parent: ParentRef {
                    branch_id: parent_branch,
                    turn: parent_turn,
                    attempt: parent_attempt,
                },
                prompt,
                lease,
            },
        )?;
        let attempt = state
            .branches
            .iter()
            .filter(|branch| branch.turn == target)
            .map(|branch| branch.attempt)
            .max()
            .unwrap_or(1);
        Ok(self.make_reservation(ReservationInput {
            branch_id: &branch_id,
            turn: target,
            attempt,
            retry: false,
            prompt,
            prior,
            owner: lease.owner.clone(),
        }))
    }

    fn fresh_parent(
        state: &SessionState,
        current: Option<usize>,
        requested: Option<u32>,
        target: u32,
    ) -> (Option<String>, u32, u32) {
        match (current, requested) {
            (Some(index), Some(_)) => {
                let lineage = Self::lineage(state, index);
                Self::predecessor_parent(&state.branches, &lineage, target)
            }
            (Some(index), None) => {
                let parent = &state.branches[index];
                (Some(parent.branch_id.clone()), parent.turn, parent.attempt)
            }
            (None, _) => (None, 0, 0),
        }
    }

    fn predecessor_parent(
        branches: &[BranchRecord],
        lineage: &[usize],
        target: u32,
    ) -> (Option<String>, u32, u32) {
        match Self::predecessor_at(branches, lineage, target) {
            Some(index) => {
                let parent = &branches[index];
                (Some(parent.branch_id.clone()), parent.turn, parent.attempt)
            }
            None => (None, 0, 0),
        }
    }

    /// Allocate the next branch id and append the pending reservation for a retry/fork.
    fn push_new_branch(
        &self,
        state: &mut SessionState,
        input: NewBranch<'_>,
    ) -> Result<String, StoreError> {
        let seq = state
            .next_branch_seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("branch sequence overflow".to_string()))?;
        state.next_branch_seq = seq;
        let branch_id = format!("b{seq}");
        let attempt = state
            .branches
            .iter()
            .filter(|branch| branch.turn == input.target)
            .map(|branch| branch.attempt)
            .max()
            .map(|attempt| {
                attempt
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Invalid("attempt overflow".to_string()))
            })
            .transpose()?
            .unwrap_or(1);
        let branch = BranchRecord {
            branch_id: branch_id.clone(),
            turn: input.target,
            attempt,
            parent_branch: input.parent.branch_id,
            parent_turn: input.parent.turn,
            parent_attempt: input.parent.attempt,
            prompt: input.prompt.to_string(),
            digest: crate::limits::prompt_digest(input.prompt),
            lifecycle: Lifecycle::Pending,
            rounds: Vec::new(),
            summary: String::new(),
            error: String::new(),
            owner: input.lease.owner.clone(),
            reserved_at: input.lease.now,
            lease_expiry: input.lease.lease_end,
        };
        state.branches.push(branch.clone());
        self.append_event(log::Event::BranchReserved {
            cwd: state.cwd.clone(),
            cwd_dev: state.cwd_dev,
            cwd_ino: state.cwd_ino,
            branch,
            next_branch_seq: seq,
        })?;
        Ok(branch_id)
    }

    /// Build the reserved-request value for a fresh reservation.
    fn make_reservation(&self, input: ReservationInput<'_>) -> ReservedRequest {
        ReservedRequest {
            branch_id: input.branch_id.to_string(),
            turn: input.turn,
            attempt: input.attempt,
            replay: false,
            retry: input.retry,
            rounds: Vec::new(),
            summary: String::new(),
            prompt: input.prompt.to_string(),
            history: input.prior,
            owner: input.owner,
        }
    }

    /// The retry path inside start_request builds its reservation after pushing the branch.
    fn make_retry_reservation(
        &self,
        branch_id: &str,
        state: &SessionState,
        target: u32,
        prompt: &str,
        prior: Vec<HistoryTurn>,
        owner: String,
    ) -> ReservedRequest {
        let attempt = state
            .branches
            .iter()
            .filter(|b| b.turn == target)
            .map(|b| b.attempt)
            .max()
            .unwrap_or(1);
        self.make_reservation(ReservationInput {
            branch_id,
            turn: target,
            attempt,
            retry: true,
            prompt,
            prior,
            owner,
        })
    }
}
