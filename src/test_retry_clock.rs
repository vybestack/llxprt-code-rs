//! Test-only clock boundary: real I/O runs on real time; host backoff is paused.
//! A response cannot spend the next retry's virtual budget while the OS schedules
//! its loopback peer. Nothing in this module is compiled into the stock binary.
use crate::adapter::{ChatBackend, ModelFuture};
use std::cell::Cell;

pub(crate) struct RetryClock<B> {
    inner: B,
    paused: Cell<bool>,
}
impl<B> RetryClock<B> {
    pub(crate) fn new(inner: B) -> Self {
        Self {
            inner,
            paused: Cell::new(false),
        }
    }
}
impl<B: ChatBackend> ChatBackend for RetryClock<B> {
    fn provider_class(&self) -> crate::context_gateway::ProviderClass {
        self.inner.provider_class()
    }
    fn supports_context_handles(&self) -> bool {
        self.inner.supports_context_handles()
    }
    fn max_output_tokens(&self) -> Option<u64> {
        self.inner.max_output_tokens()
    }
    fn set_context_epoch(&self, epoch: u64) {
        self.inner.set_context_epoch(epoch)
    }
    fn request_contract(&self) -> Option<crate::telemetry::bounds::RequestContract> {
        self.inner.request_contract()
    }
    fn streams_response(&self) -> bool {
        self.inner.streams_response()
    }
    fn stream_interruption(&self) -> Option<usize> {
        self.inner.stream_interruption()
    }
    fn cache_observation(&self) -> Option<crate::cache_output::Snapshot> {
        self.inner.cache_observation()
    }
    fn tool_error_prefix(&self) -> &'static str {
        self.inner.tool_error_prefix()
    }
    fn request_calls(&self) -> usize {
        self.inner.request_calls()
    }
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        tools: &'a [crate::tools::ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            if self.paused.replace(false) {
                tokio::time::resume();
            }
            let result = self.inner.request(requests, tools).await;
            tokio::time::pause();
            self.paused.set(true);
            result
        })
    }
}
