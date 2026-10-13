//! Closed provider-turn state machine shared by conversation and management.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderClass {
    /// Complete request state is supplied by the caller; no hidden conversation.
    StatelessAttested,
    /// The transport must implement identity-preserving dispatch/status lookup.
    IdempotentIdentity,
    /// No delivery knowledge after process death; an intent is never resent.
    AtMostOneAttempt,
    /// Hidden or unattested state: unattended execution is refused.
    Unattested,
}
impl ProviderClass {
    pub fn unattended(self) -> bool {
        matches!(self, Self::StatelessAttested | Self::IdempotentIdentity)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnKind {
    Conversation,
    Management,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Intent,
    Pending,
    Admitted,
    Rejected,
    Indeterminate,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendIntent {
    pub identity: String,
    pub branch: String,
    pub owner: String,
    pub epoch: u64,
    pub context_version: u64,
    pub request_hash: String,
    pub contract_hash: String,
    pub class: ProviderClass,
    pub kind: TurnKind,
    pub allowance: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIntent {
    pub id: String,
    pub tool: String,
    pub args: String,
    pub read_only: bool,
    pub completion: Option<(bool, String)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRecord {
    pub intent: SendIntent,
    pub status: Status,
    /// Bounded, validated reply. A response and its tool intents share one commit.
    pub response: Option<Vec<u8>>,
    pub executions: Vec<ExecutionIntent>,
}
/// The sole mutation vocabulary. Reducer checks precede durable publication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Intent(SendIntent),
    Stage {
        identity: String,
        bytes: Vec<u8>,
    },
    Admit {
        identity: String,
        executions: Vec<ExecutionIntent>,
    },
    Reject {
        identity: String,
    },
    Complete {
        identity: String,
        call: String,
        ok: bool,
        result: String,
    },
    Recover {
        branch: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    #[error("provider turn has an illegal transition")]
    Transition,
    #[error("provider turn exceeds pre-send allowance")]
    Allowance,
    #[error("provider identity/hash is invalid")]
    Identity,
    #[error("provider response is indeterminate; request must not be resent")]
    Indeterminate,
    #[error("provider class does not support unattended operation")]
    Unsupported,
}
/// Intentionally neither Clone nor serializable. Only the live writer receives it.
pub struct SendPermit {
    intent: SendIntent,
}
impl SendPermit {
    pub(crate) fn new(intent: SendIntent) -> Self {
        Self { intent }
    }
    pub fn intent(&self) -> &SendIntent {
        &self.intent
    }
    pub fn consume(self) -> SendIntent {
        self.intent
    }
}
fn record<'a>(records: &'a mut [TurnRecord], id: &str) -> Result<&'a mut TurnRecord, GatewayError> {
    records
        .iter_mut()
        .find(|r| r.intent.identity == id)
        .ok_or(GatewayError::Identity)
}
pub(crate) fn hash_valid(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Replay is deterministic; recovery never manufactures a send permit.
pub fn apply(records: &mut Vec<TurnRecord>, command: &Command) -> Result<(), GatewayError> {
    match command {
        Command::Intent(intent) => insert(records, intent),
        Command::Stage { identity, bytes } => {
            let r = record(records, identity)?;
            if r.status != Status::Intent {
                return Err(GatewayError::Transition);
            }
            if bytes.len() > r.intent.allowance {
                return Err(GatewayError::Allowance);
            }
            r.response = Some(bytes.clone());
            r.status = Status::Pending;
            Ok(())
        }
        Command::Admit {
            identity,
            executions,
        } => admit(record(records, identity)?, executions),
        Command::Reject { identity } => {
            let r = record(records, identity)?;
            if !matches!(r.status, Status::Intent | Status::Pending) {
                return Err(GatewayError::Transition);
            }
            r.response = None;
            r.status = Status::Rejected;
            Ok(())
        }
        Command::Complete {
            identity,
            call,
            ok,
            result,
        } => complete(record(records, identity)?, call, *ok, result),
        Command::Recover { branch } => {
            for r in records.iter_mut().filter(|r| r.intent.branch == *branch) {
                if matches!(r.status, Status::Intent | Status::Pending) {
                    r.status = Status::Indeterminate;
                    r.response = None;
                }
                for call in &mut r.executions {
                    if call.completion.is_none() {
                        call.completion = Some((
                            false,
                            "indeterminate execution; not repeated after restart".into(),
                        ));
                    }
                }
            }
            Ok(())
        }
    }
}
fn insert(records: &mut Vec<TurnRecord>, intent: &SendIntent) -> Result<(), GatewayError> {
    if intent.identity.is_empty()
        || intent.identity.len() > 256
        || intent.owner.is_empty()
        || intent.branch.is_empty()
        || intent.epoch == 0
        || intent.allowance == 0
        || !hash_valid(&intent.request_hash)
        || !hash_valid(&intent.contract_hash)
        || records.iter().any(|r| r.intent.identity == intent.identity)
    {
        return Err(GatewayError::Identity);
    }
    records.push(TurnRecord {
        intent: intent.clone(),
        status: Status::Intent,
        response: None,
        executions: Vec::new(),
    });
    Ok(())
}
fn admit(r: &mut TurnRecord, executions: &[ExecutionIntent]) -> Result<(), GatewayError> {
    if r.status != Status::Pending || r.response.is_none() {
        return Err(GatewayError::Transition);
    }
    let mut ids = std::collections::BTreeSet::new();
    if executions.iter().any(|c| {
        c.id.is_empty() || c.tool.is_empty() || c.completion.is_some() || !ids.insert(&c.id)
    }) {
        return Err(GatewayError::Identity);
    }
    r.executions = executions.to_vec();
    r.status = Status::Admitted;
    Ok(())
}
fn complete(r: &mut TurnRecord, id: &str, ok: bool, result: &str) -> Result<(), GatewayError> {
    if r.status != Status::Admitted {
        return Err(GatewayError::Transition);
    }
    let c = r
        .executions
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or(GatewayError::Identity)?;
    if c.completion.is_some() {
        return Err(GatewayError::Transition);
    }
    c.completion = Some((ok, result.into()));
    Ok(())
}

/// Snapshot validation is as strict as event replay; a forged snapshot cannot
/// skip the pre-send bound or release a reply without its complete intent set.
pub fn validate_records(records: &[TurnRecord]) -> Result<(), GatewayError> {
    let mut identities = std::collections::BTreeSet::new();
    for r in records {
        if !identities.insert(&r.intent.identity)
            || !hash_valid(&r.intent.request_hash)
            || !hash_valid(&r.intent.contract_hash)
            || r.intent.epoch == 0
            || r.intent.allowance == 0
            || r.intent.branch.is_empty()
            || r.intent.owner.is_empty()
        {
            return Err(GatewayError::Identity);
        }
        match r.status {
            Status::Intent | Status::Rejected | Status::Indeterminate if r.response.is_some() => {
                return Err(GatewayError::Transition)
            }
            Status::Pending | Status::Admitted if r.response.is_none() => {
                return Err(GatewayError::Transition)
            }
            _ => {}
        }
        if r.response
            .as_ref()
            .is_some_and(|b| b.len() > r.intent.allowance)
        {
            return Err(GatewayError::Allowance);
        }
        if !r.executions.is_empty() && r.status != Status::Admitted {
            return Err(GatewayError::Transition);
        }
        let mut ids = std::collections::BTreeSet::new();
        if r.executions
            .iter()
            .any(|c| c.id.is_empty() || c.tool.is_empty() || !ids.insert(&c.id))
        {
            return Err(GatewayError::Identity);
        }
    }
    Ok(())
}
