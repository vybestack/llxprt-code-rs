//! The only agent-to-provider dispatch boundary, for every transport attempt.
use super::*;
use crate::context_gateway::{
    self as gateway, Capability, Command, RenderPublication, SendIntent, TurnKind,
};

impl Turn<'_> {
    pub(super) fn gateway_round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        reattempt: bool,
        context: (&SessionStore, &ReservedRequest),
        epoch: u64,
    ) -> Result<LlmResult, RoundFailure> {
        let (store, reserved) = context;
        let quoted = crate::context_kernel::security::quote_requests(requests);
        let requests = quoted.as_slice();
        self.check_environment_projection(requests, tools)?;
        gateway::legal_requests(requests).map_err(|e| RoundFailure::Model(e.to_string()))?;
        let publication = self.render_publication(tools)?;
        let changed = store
            .publish_render_contract(reserved, publication.clone())
            .map_err(RoundFailure::RetryStore)?;
        if changed {
            self.calibration.borrow_mut().clear();
            self.check_quoted_request(requests, tools)?;
        }
        store
            .record_provider_boundary()
            .map_err(RoundFailure::RetryStore)?;
        let intent = SendIntent {
            identity: String::new(),
            branch: reserved.branch_id.clone(),
            owner: reserved.owner.clone(),
            epoch,
            context_version: epoch,
            request_hash: publication
                .request_hash(requests, epoch)
                .map_err(|e| RoundFailure::Model(e.to_string()))?,
            contract_hash: publication.contract_hash,
            class: self.backend.provider_class(),
            kind: TurnKind::Conversation,
            allowance: crate::limits::MAX_TURN_ASSISTANT_BYTES
                .saturating_add(crate::limits::MAX_TURN_ARGS_BYTES)
                .saturating_mul(6),
        };
        let permit = store
            .provider_intent(reserved, intent)
            .map_err(RoundFailure::RetryStore)?;
        let admitted = store
            .admit_send(reserved, permit)
            .map_err(RoundFailure::RetryStore)?;
        crate::context_kernel::security::legal_quotes(requests)
            .map_err(|e| RoundFailure::Model(e.to_string()))?;
        let reply = match self.round_once(requests, tools, reattempt) {
            Ok(reply) => reply,
            Err(error) => {
                store
                    .provider_command(
                        reserved,
                        Command::Reject {
                            identity: admitted.identity,
                        },
                    )
                    .map_err(RoundFailure::RetryStore)?;
                return Err(error);
            }
        };
        if is_output_truncation(&reply) {
            store
                .provider_command(
                    reserved,
                    Command::Reject {
                        identity: admitted.identity,
                    },
                )
                .map_err(RoundFailure::RetryStore)?;
            return Ok(reply);
        }
        // Complete structural validation precedes durable execution admission.
        finish_check(&reply).map_err(RoundFailure::FinishReason)?;
        validate_calls(&mut std::collections::HashSet::new(), &reply)
            .map_err(RoundFailure::InvalidToolCall)?;
        // This live lease check suppresses a response from an old owner before any
        // response bytes, execution intent, transcript, or world effect are released.
        self.admit_reply(store, reserved, &admitted.identity, &reply)?;
        Ok(reply)
    }
    fn check_environment_projection(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<(), RoundFailure> {
        crate::context_kernel::security::legal_quotes(requests)
            .map_err(|e| RoundFailure::Model(e.to_string()))?;
        if requests
            .iter()
            .flat_map(|r| &r.parts)
            .any(|p| matches!(p, serdes_ai::core::ModelRequestPart::ToolReturn(_)))
        {
            self.check_quoted_request(requests, tools)?;
        }
        Ok(())
    }
    fn check_quoted_request(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<(), RoundFailure> {
        crate::context_kernel::security::legal_quotes(requests)
            .map_err(|e| RoundFailure::Model(e.to_string()))?;
        if estimate_request_bytes(requests)
            .saturating_add(crate::adapter::estimate_tool_schema_bytes(tools))
            > materialization_budget(self.context_limit)
        {
            return Err(RoundFailure::RequestBound {
                limit: self.context_limit.unwrap_or(0),
            });
        }
        Ok(())
    }
    fn admit_reply(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        identity: &str,
        reply: &LlmResult,
    ) -> Result<(), RoundFailure> {
        let bytes = serde_json::to_vec(&reply).map_err(|e| RoundFailure::Model(e.to_string()))?;
        let executions = reply
            .calls
            .iter()
            .map(|call| gateway::ExecutionIntent {
                id: call.id.clone(),
                tool: call.name.clone(),
                args: call.args_json.clone(),
                read_only: crate::context_kernel::branch::ToolContract {
                    name: call.name.clone(),
                    read_only: true,
                    reads: Default::default(),
                    writes: Default::default(),
                }
                .permits_branch(),
                completion: None,
            })
            .collect();
        store
            .admit_provider_response(reserved, identity, bytes, executions)
            .map_err(RoundFailure::RetryStore)?;
        Ok(())
    }
    fn render_publication(
        &self,
        tools: &[crate::tools::ToolSpec],
    ) -> Result<RenderPublication, RoundFailure> {
        let contract = self.backend.request_contract();
        let capability = Capability {
            version: contract.as_ref().map_or(1, |c| c.render_version),
            serializer_version: contract.as_ref().map_or(1, |c| c.serializer_version),
            margin_version: contract.as_ref().map_or(1, |c| c.margin_version),
            tokenizer_version: contract.as_ref().and_then(|c| c.tokenizer_version.clone()),
            placeholders: self.backend.supports_context_handles(),
            provider_instructions_hash: contract
                .as_ref()
                .map_or_else(|| gateway::digest(b"unspecified"), |c| c.identity.clone()),
            profile_hash: gateway::digest(
                &serde_json::to_vec(&(contract, self.context_limit))
                    .map_err(|e| RoundFailure::Model(e.to_string()))?,
            ),
            max_output_tokens: self.backend.max_output_tokens(),
        };
        let declarations: Vec<_> = tools.iter().map(crate::adapter::schema_for).collect();
        let bytes =
            serde_json::to_vec(&declarations).map_err(|e| RoundFailure::Model(e.to_string()))?;
        RenderPublication::new(capability, &bytes).map_err(|e| RoundFailure::Model(e.to_string()))
    }
}
