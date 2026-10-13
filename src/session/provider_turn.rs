//! Intent-before-send and atomic response admission under the existing session lock.
//! Payloads are bounded validated provider replies; no partial transport text is stored.
use super::*;
use crate::context_gateway::{
    self as gateway, Command, ExecutionIntent, SendIntent, SendPermit, Status,
};

pub(crate) struct Recovery {
    pub rounds: Vec<RoundRecord>,
    pub replies: Vec<Vec<u8>>,
}
impl SessionStore {
    pub(crate) fn publish_render_contract(
        &self,
        reserved: &ReservedRequest,
        publication: gateway::RenderPublication,
    ) -> Result<bool, StoreError> {
        self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            if state.render_contracts.last() == Some(&publication) {
                return Ok(false);
            }
            self.append_event(log::Event::RenderContract {
                branch_id: reserved.branch_id.clone(),
                owner: reserved.owner.clone(),
                publication,
            })?;
            Ok(!state.render_contracts.is_empty())
        })
    }
    pub(crate) fn provider_intent(
        &self,
        reserved: &ReservedRequest,
        mut intent: SendIntent,
    ) -> Result<SendPermit, StoreError> {
        self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            intent.owner = reserved.owner.clone();
            intent.branch = reserved.branch_id.clone();
            intent.epoch = state.provider_epoch;
            intent.identity = match intent.kind {
                gateway::TurnKind::Conversation => {
                    format!("{}:{}", self.session_id, state.provider_turns.len() + 1)
                }
                gateway::TurnKind::Management => format!(
                    "{}:{}:management:{}",
                    self.session_id,
                    reserved.branch_id,
                    gateway::digest(intent.identity.as_bytes())
                ),
            };
            let mut candidate = state.provider_turns.clone();
            gateway::apply(&mut candidate, &Command::Intent(intent.clone()))
                .map_err(protocol_error)?;
            self.append_event(log::Event::Provider {
                branch_id: reserved.branch_id.clone(),
                owner: reserved.owner.clone(),
                command: Command::Intent(intent.clone()),
            })?;
            self.publish_provider_index()?;
            Ok(SendPermit::new(intent))
        })
    }
    /// Send admission is fenced again after all pre-send serialization and publication.
    pub(crate) fn admit_send(
        &self,
        reserved: &ReservedRequest,
        permit: SendPermit,
    ) -> Result<SendIntent, StoreError> {
        self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            let intent = permit.consume();
            if !state
                .provider_turns
                .iter()
                .any(|r| r.intent == intent && r.status == Status::Intent)
            {
                return Err(protocol_error(gateway::GatewayError::Transition));
            }
            Ok(intent)
        })
    }
    pub(crate) fn provider_command(
        &self,
        reserved: &ReservedRequest,
        command: Command,
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            let mut candidate = state.provider_turns.clone();
            gateway::apply(&mut candidate, &command).map_err(protocol_error)?;
            self.append_event(log::Event::Provider {
                branch_id: reserved.branch_id.clone(),
                owner: reserved.owner.clone(),
                command,
            })?;
            self.publish_provider_index()
        })
    }
    /// Validated response and the complete per-call intent set have one visibility point.
    pub(crate) fn admit_provider_response(
        &self,
        reserved: &ReservedRequest,
        identity: &str,
        bytes: Vec<u8>,
        executions: Vec<ExecutionIntent>,
    ) -> Result<(), StoreError> {
        self.provider_command(
            reserved,
            Command::Stage {
                identity: identity.into(),
                bytes,
            },
        )?;
        self.provider_command(
            reserved,
            Command::Admit {
                identity: identity.into(),
                executions,
            },
        )
    }
    pub(crate) fn complete_provider_tool(
        &self,
        reserved: &ReservedRequest,
        call: &ToolCallRecord,
    ) -> Result<(), StoreError> {
        let identity = self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            state
                .provider_turns
                .iter()
                .rev()
                .find(|r| {
                    r.intent.branch == reserved.branch_id
                        && r.status == Status::Admitted
                        && r.executions.iter().any(|c| c.id == call.id)
                })
                .map(|r| r.intent.identity.clone())
                .ok_or_else(|| StoreError::Invalid("tool has no provider execution intent".into()))
        })?;
        self.provider_command(
            reserved,
            Command::Complete {
                identity,
                call: call.id.clone(),
                ok: call.ok,
                result: call.result.clone(),
            },
        )
    }
    /// Fresh owner refuses any unresolved send; missing tool completions become paired
    /// indeterminate results. No repeat of a side-effecting execution is authorized.
    pub(crate) fn recover_provider_turns(
        &self,
        reserved: &ReservedRequest,
    ) -> Result<Recovery, StoreError> {
        self.locked(|| {
            let state = self.read()?;
            self.live_branch(&state, reserved)?;
            let old: Vec<_> = state
                .provider_turns
                .iter()
                .filter(|r| {
                    r.intent.branch == reserved.branch_id && r.intent.owner != reserved.owner
                })
                .collect();
            if old.is_empty() {
                return Ok(Recovery {
                    rounds: Vec::new(),
                    replies: Vec::new(),
                });
            }
            let unresolved = old.iter().any(|r| {
                matches!(
                    r.status,
                    Status::Intent | Status::Pending | Status::Indeterminate
                )
            });
            self.append_event(log::Event::Provider {
                branch_id: reserved.branch_id.clone(),
                owner: reserved.owner.clone(),
                command: Command::Recover {
                    branch: reserved.branch_id.clone(),
                },
            })?;
            if unresolved {
                return Err(protocol_error(gateway::GatewayError::Indeterminate));
            }
            recovered_calls(old)
        })
    }
}
fn protocol_error(error: gateway::GatewayError) -> StoreError {
    StoreError::Invalid(error.to_string())
}

fn recovered_calls(old: Vec<&gateway::TurnRecord>) -> Result<Recovery, StoreError> {
    let mut rounds = Vec::new();
    let mut replies = Vec::new();
    for r in old.into_iter().filter(|r| {
        r.status == Status::Admitted && r.intent.kind == gateway::TurnKind::Conversation
    }) {
        replies.push(
            r.response
                .clone()
                .ok_or_else(|| StoreError::Corrupt("admitted reply missing".into()))?,
        );
        if r.executions.is_empty() {
            continue;
        }
        rounds.push(RoundRecord {
            assistant: String::new(),
            thinking: String::new(),
            continuation: None,
            calls: r
                .executions
                .iter()
                .map(|call| {
                    let (ok, result) = call.completion.clone().unwrap_or((
                        false,
                        "indeterminate execution; not repeated after restart".into(),
                    ));
                    ToolCallRecord {
                        id: call.id.clone(),
                        name: call.tool.clone(),
                        args: call.args.clone(),
                        ok,
                        refused: false,
                        result,
                        result_live: String::new(),
                    }
                })
                .collect(),
        });
    }
    Ok(Recovery { rounds, replies })
}

#[cfg(test)]
mod tests;

impl SessionStore {
    fn publish_provider_index(&self) -> Result<(), StoreError> {
        // Advisory index is derived from the committed log, never the replay authority.
        let state = self.read()?;
        let context = ensure_private_subdir(&self.dir, "context")
            .map_err(|_| StoreError::Io("open provider index directory".into()))?;
        let bytes = serde_json::to_vec(&state.provider_turns)
            .map_err(|_| StoreError::Invalid("provider index encoding".into()))?;
        crate::safe_file::publish_artifact(&context, "provider-index", &bytes, open_regular_at)
            .map_err(|_| StoreError::Io("publish provider index".into()))
    }
}
