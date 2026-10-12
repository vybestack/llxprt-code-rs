//! Bind private continuation to the resolved route/model; never replay on a switch.
use crate::adapter::{ChatBackend, ModelFuture};
use serdes_ai::core::{ModelRequest, ModelRequestPart};
use sha2::{Digest, Sha256};

pub(super) struct BoundBackend {
    inner: Box<dyn ChatBackend>,
    binding: String,
    contract: crate::telemetry::bounds::RequestContract,
}

impl BoundBackend {
    pub(super) fn new(inner: Box<dyn ChatBackend>, profile: &crate::profile::Profile) -> Self {
        let identity = format!(
            "{}\0{:?}\0{}\0{}",
            profile.provider_selection.as_str(),
            profile.api_selection,
            profile.model,
            profile
                .ephemeral
                .base_url
                .as_ref()
                .map_or("default", |u| u.full())
        );
        let contract_identity = format!(
            "{identity}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}",
            profile.model_params,
            profile.anthropic_settings,
            profile.codex_settings,
            profile.openai_responses_settings,
            profile.ephemeral.max_output_tokens
        );
        let anthropic = profile.provider_selection.as_str() == "anthropic";
        let contract = crate::telemetry::bounds::RequestContract {
            identity: format!("{:x}", Sha256::digest(contract_identity.as_bytes())),
            serializer_version: 1,
            render_version: 1,
            margin_version: 1,
            tokenizer: None,
            tokenizer_version: None,
            input_accounting: if anthropic {
                "input_excludes_cache_reads_and_creation"
            } else {
                "input_includes_cache_reads"
            }
            .into(),
        };
        Self {
            inner,
            contract,
            binding: format!("{:x}", Sha256::digest(identity.as_bytes())),
        }
    }
}

impl ChatBackend for BoundBackend {
    fn request_contract(&self) -> Option<crate::telemetry::bounds::RequestContract> {
        Some(self.contract.clone())
    }
    fn streams_response(&self) -> bool {
        self.inner.streams_response()
    }
    fn stream_interruption(&self) -> Option<usize> {
        self.inner.stream_interruption()
    }
    fn tool_error_prefix(&self) -> &'static str {
        self.inner.tool_error_prefix()
    }
    fn request_calls(&self) -> usize {
        self.inner.request_calls()
    }
    fn request<'a>(
        &'a self,
        requests: &'a [ModelRequest],
        tools: &'a [crate::tools::ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            // The host carries the binding as private response metadata. Check before
            // touching the transport, then remove it from provider-visible blocks.
            let mut requests = requests.to_vec();
            for part in requests.iter_mut().flat_map(|r| &mut r.parts) {
                if let ModelRequestPart::ModelResponse(response) = part {
                    if let Some(details) = &mut response.vendor_details {
                        if let Some(binding) = details.get("host_reasoning_binding") {
                            if binding.as_str() != Some(&self.binding) {
                                return Err(
                                    "reasoning continuation route/model mismatch; no request sent"
                                        .into(),
                                );
                            }
                        }
                        if let Some(details) = details.as_object_mut() {
                            details.remove("host_reasoning_binding");
                        }
                    }
                }
            }
            let mut result = self.inner.request(&requests, tools).await?;
            if let Some(state) = &mut result.continuation {
                state.bind(&self.binding);
            }
            Ok(result)
        })
    }
}
