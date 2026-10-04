use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct RunCache {
    pub(crate) calls: Option<u64>,
    pub(crate) measured_calls: Option<u64>,
    pub(crate) measured_input_tokens: Option<u64>,
    pub(crate) measured_cached_tokens: Option<u64>,
    pub(crate) aggregate_valid: bool,
    pub(crate) hit_ratio: Option<f64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Observation {
    pub(crate) event: &'static str,
    pub(crate) call: Option<u64>,
    pub(crate) reported_input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) uncached_input_tokens: Option<u64>,
    pub(crate) cache_creation_input_tokens: Option<u64>,
    pub(crate) total_input_tokens: Option<u64>,
    pub(crate) input_accounting: &'static str,
}
