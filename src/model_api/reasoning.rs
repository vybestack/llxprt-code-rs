//! Bind private continuation to the resolved route/model; never replay on a switch.
use crate::adapter::{ChatBackend, ModelFuture};
use serdes_ai::core::{ModelRequest, ModelRequestPart};
use sha2::{Digest, Sha256};

pub(super) struct BoundBackend {
    inner: Box<dyn ChatBackend>,
    binding: String,
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
        Self {
            inner,
            binding: format!("{:x}", Sha256::digest(identity.as_bytes())),
        }
    }
}

impl ChatBackend for BoundBackend {
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
