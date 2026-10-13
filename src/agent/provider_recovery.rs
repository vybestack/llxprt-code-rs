//! Resume only committed replies and completions. An unresolved provider intent
//! terminates indeterminate; no transport retry can be synthesized by replay.
use super::*;
impl Turn<'_> {
    pub(super) fn resume_or_begin(
        &self,
        store: &SessionStore,
        reserved: &mut ReservedRequest,
        mut requests: Vec<serdes_ai::core::ModelRequest>,
        tools: &[crate::tools::ToolSpec],
    ) -> Result<AttemptState, AgentError> {
        let mut recovered = store.recover_provider_turns(reserved).map_err(|error| {
            self.dead_coded(
                store,
                reserved,
                crate::envelope::Code::Session,
                "provider-indeterminate",
                &error.to_string(),
                &reserved.rounds,
            )
        })?;
        if recovered.rounds.is_empty() && recovered.replies.is_empty() {
            return self.begin_attempt(store, reserved, requests, tools);
        }
        let final_reply = restore_responses(&mut recovered)?;
        let rounds = recovered.rounds;
        for round in &rounds {
            requests.extend(persisted_round_requests(round));
        }
        let ids = rounds
            .iter()
            .flat_map(|r| r.calls.iter().map(|c| c.id.clone()))
            .collect();
        let usage = TurnUsage {
            assistant_bytes: rounds
                .iter()
                .map(|r| r.assistant.len().saturating_add(r.thinking.len()))
                .sum(),
            args_bytes: rounds
                .iter()
                .flat_map(|r| &r.calls)
                .map(|c| c.args.len())
                .sum(),
            output_bytes: rounds
                .iter()
                .flat_map(|r| &r.calls)
                .map(|c| c.result.len())
                .sum(),
            total_calls: rounds.iter().map(|r| r.calls.len()).sum(),
        };
        let mut attempt = AttemptState {
            requests,
            rounds,
            current: final_reply,
            ids,
            usage,
            budget_exhausted: false,
        };
        if attempt.current.is_none() {
            self.request_next_round(store, reserved, tools, &mut attempt)?;
        }
        Ok(attempt)
    }
}

fn restore_responses(
    recovered: &mut crate::session::provider_turn::Recovery,
) -> Result<Option<LlmResult>, AgentError> {
    let mut final_reply = None;
    for bytes in &recovered.replies {
        let reply: LlmResult = serde_json::from_slice(bytes).map_err(|_| {
            AgentError::new(
                crate::envelope::Code::Session,
                "provider-recovery",
                "admitted reply corrupt",
            )
        })?;
        if reply.calls.is_empty() {
            final_reply = Some(reply);
        } else {
            final_reply = None;
            if let Some(round) = recovered
                .rounds
                .iter_mut()
                .find(|r| r.calls.first().map(|c| &c.id) == reply.calls.first().map(|c| &c.id))
            {
                round.assistant = reply.text;
                round.thinking = reply.thinking;
                round.continuation = reply.continuation;
            }
        }
    }
    Ok(final_reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_gateway::{self as gateway, ProviderClass, SendIntent, TurnKind};
    #[test]
    fn fresh_agent_finishes_admitted_reply_without_another_provider_request() {
        let root = tempfile::tempdir().unwrap();
        let store =
            SessionStore::load_at(&crate::session::SessionId::fresh(), root.path()).unwrap();
        let reserved = store.start_request(None, None, "P", root.path()).unwrap();
        let reply = super::super::naming_recovery_tests::reply(vec![]);
        let permit = store
            .provider_intent(
                &reserved,
                SendIntent {
                    identity: String::new(),
                    branch: reserved.branch_id.clone(),
                    owner: reserved.owner.clone(),
                    epoch: 1,
                    context_version: 1,
                    request_hash: gateway::digest(b"request"),
                    contract_hash: gateway::digest(b"contract"),
                    class: ProviderClass::AtMostOneAttempt,
                    kind: TurnKind::Conversation,
                    allowance: 4096,
                },
            )
            .unwrap();
        let identity = store.admit_send(&reserved, permit).unwrap().identity;
        store
            .admit_provider_response(
                &reserved,
                &identity,
                serde_json::to_vec(&reply).unwrap(),
                vec![],
            )
            .unwrap();
        let mut state = store.snapshot().unwrap();
        state.branches[0].reserved_at = 1;
        state.branches[0].lease_expiry = 2;
        store.replace_snapshot(&state).unwrap();
        let fresh = SessionStore::load_at(
            &crate::session::SessionId::parse(&state.session_id).unwrap(),
            root.path(),
        )
        .unwrap();
        let new = fresh.start_request(None, None, "P", root.path()).unwrap();
        let agent = CodingAgent::with_backend(
            Box::new(super::super::tests::MockBackend::new(vec![])),
            root.path().into(),
            false,
        );
        assert_eq!(agent.run(&fresh, &new).unwrap().summary, "done");
        assert_eq!(agent.model_calls(), 0);
        assert_eq!(
            fresh.snapshot().unwrap().branches[0].lifecycle,
            crate::session::Lifecycle::Completed
        );
    }
    #[test]
    fn unattended_unknown_backend_refuses_before_intent_or_send() {
        let root = tempfile::tempdir().unwrap();
        let store =
            SessionStore::load_at(&crate::session::SessionId::fresh(), root.path()).unwrap();
        let reserved = store.start_request(None, None, "P", root.path()).unwrap();
        let agent = CodingAgent::with_backend(
            Box::new(super::super::tests::MockBackend::new(vec![])),
            root.path().into(),
            false,
        )
        .with_unattended(true);
        assert_eq!(
            agent.run(&store, &reserved).unwrap_err().key,
            "provider-class"
        );
        assert_eq!(agent.model_calls(), 0);
        assert!(store.snapshot().unwrap().provider_turns.is_empty());
    }
}
