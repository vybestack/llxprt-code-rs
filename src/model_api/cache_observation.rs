//! Provider-reported prompt-cache accounting at the real request boundary.
//!
//! OpenAI input includes cached reads. Anthropic input excludes reads and writes.
//! Missing counters are never replaced with zero. The ratio covers only calls
//! reporting a complete input denominator and cached-read count, weighted by tokens.
use std::cell::RefCell;

use crate::adapter::{ChatBackend, LlmResult, LlmUsage};
use crate::tools::ToolSpec;
use serde::Serialize;
use serdes_ai::core::ModelRequest;

#[derive(Clone, Copy)]
pub(super) enum InputAccounting {
    Inclusive,
    Anthropic,
}

#[derive(Default, Serialize)]
struct RunCache {
    calls: u64,
    measured_calls: u64,
    measured_input_tokens: u64,
    measured_cached_tokens: u64,
    hit_ratio: Option<f64>,
}

#[derive(Serialize)]
struct Observation {
    event: &'static str,
    call: u64,
    reported_input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    uncached_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    total_input_tokens: Option<u64>,
    input_accounting: &'static str,
}

impl InputAccounting {
    fn observe(self, usage: &LlmUsage, call: u64) -> Observation {
        let (total, uncached, accounting) = match self {
            Self::Inclusive => (
                usage.request_tokens,
                usage
                    .request_tokens
                    .zip(usage.cache_read_tokens)
                    .and_then(|(input, read)| input.checked_sub(read)),
                "input_includes_cache_reads",
            ),
            Self::Anthropic => {
                let total = usage
                    .request_tokens
                    .zip(usage.cache_creation_tokens)
                    .zip(usage.cache_read_tokens)
                    .and_then(|((input, creation), read)| {
                        input
                            .checked_add(creation)
                            .and_then(|n| n.checked_add(read))
                    });
                (
                    total,
                    usage
                        .request_tokens
                        .zip(usage.cache_creation_tokens)
                        .and_then(|(input, creation)| input.checked_add(creation)),
                    "input_excludes_cache_reads_and_creation",
                )
            }
        };
        Observation {
            event: "prompt_cache_call",
            call,
            reported_input_tokens: usage.request_tokens,
            cached_input_tokens: usage.cache_read_tokens,
            uncached_input_tokens: uncached,
            cache_creation_input_tokens: usage.cache_creation_tokens,
            total_input_tokens: total,
            input_accounting: accounting,
        }
    }
}

impl RunCache {
    fn record(&mut self, accounting: InputAccounting, usage: &LlmUsage) -> Observation {
        self.calls += 1;
        let observation = accounting.observe(usage, self.calls);
        if let Some((input, read)) = observation
            .total_input_tokens
            .zip(observation.cached_input_tokens)
            .filter(|(input, read)| read <= input)
        {
            self.measured_calls += 1;
            self.measured_input_tokens = self
                .measured_input_tokens
                .checked_add(input)
                .expect("run input overflow");
            self.measured_cached_tokens = self
                .measured_cached_tokens
                .checked_add(read)
                .expect("run cache overflow");
            self.hit_ratio = (self.measured_input_tokens != 0)
                .then(|| self.measured_cached_tokens as f64 / self.measured_input_tokens as f64);
        }
        observation
    }
}

pub(super) struct ObservedBackend {
    inner: Box<dyn ChatBackend>,
    accounting: InputAccounting,
    run: RefCell<RunCache>,
}

impl ObservedBackend {
    pub(super) fn new(inner: Box<dyn ChatBackend>, accounting: InputAccounting) -> Self {
        Self {
            inner,
            accounting,
            run: RefCell::new(RunCache::default()),
        }
    }
}

impl ChatBackend for ObservedBackend {
    fn request(&self, requests: &[ModelRequest], tools: &[ToolSpec]) -> Result<LlmResult, String> {
        let result = self.inner.request(requests, tools)?;
        let mut run = self.run.borrow_mut();
        let observation = run.record(self.accounting, &result.usage);
        // Stderr is deliberately separate from the exactly-one-object stdout contract.
        eprintln!(
            "{}",
            serde_json::to_string(&observation).expect("cache observation serialization")
        );
        eprintln!(
            "{}",
            serde_json::json!({"event": "prompt_cache_run", "usage": &*run})
        );
        Ok(result)
    }

    fn request_calls(&self) -> usize {
        self.inner.request_calls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_zero_and_mixed_usage_preserve_denominators() {
        let mut run = RunCache::default();
        let absent = run.record(InputAccounting::Inclusive, &LlmUsage::default());
        assert_eq!(absent.uncached_input_tokens, None);
        assert_eq!(run.hit_ratio, None);
        run.record(
            InputAccounting::Inclusive,
            &LlmUsage {
                request_tokens: Some(0),
                cache_read_tokens: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(run.hit_ratio, None);
        let first = run.record(
            InputAccounting::Inclusive,
            &LlmUsage {
                request_tokens: Some(100),
                cache_read_tokens: Some(60),
                ..Default::default()
            },
        );
        assert_eq!(first.uncached_input_tokens, Some(40));
        let second = run.record(
            InputAccounting::Anthropic,
            &LlmUsage {
                request_tokens: Some(20),
                cache_creation_tokens: Some(30),
                cache_read_tokens: Some(50),
                ..Default::default()
            },
        );
        assert_eq!(second.total_input_tokens, Some(100));
        assert_eq!(second.uncached_input_tokens, Some(50));
        assert_eq!(run.hit_ratio, Some(0.55));
        assert_eq!(run.calls, 4);
        assert_eq!(run.measured_calls, 3);
        let missing = run.record(
            InputAccounting::Anthropic,
            &LlmUsage {
                request_tokens: Some(20),
                cache_read_tokens: Some(50),
                ..Default::default()
            },
        );
        assert_eq!(missing.total_input_tokens, None);
        assert_eq!(run.measured_calls, 3);
    }

    #[test]
    fn inconsistent_external_counters_do_not_panic_or_pollute_ratio() {
        let mut run = RunCache::default();
        let inconsistent = run.record(
            InputAccounting::Inclusive,
            &LlmUsage {
                request_tokens: Some(1),
                cache_read_tokens: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(inconsistent.cached_input_tokens, Some(2));
        assert_eq!(inconsistent.uncached_input_tokens, None);
        let overflow = run.record(
            InputAccounting::Anthropic,
            &LlmUsage {
                request_tokens: Some(u64::MAX),
                cache_creation_tokens: Some(1),
                cache_read_tokens: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(overflow.total_input_tokens, None);
        assert_eq!(overflow.uncached_input_tokens, None);
        assert_eq!(run.measured_calls, 0);
        assert_eq!(run.hit_ratio, None);
    }
}
