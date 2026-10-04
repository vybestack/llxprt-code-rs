//! Provider-reported prompt-cache accounting at the real request boundary.
//!
//! OpenAI input includes cached reads. Anthropic input excludes reads and writes.
//! Missing counters are never replaced with zero. The ratio covers only calls
//! reporting a complete input denominator and cached-read count, weighted by tokens.
use std::cell::RefCell;

use crate::adapter::{ChatBackend, LlmUsage};
use crate::cache_output::{Observation, RunCache, Snapshot};
use crate::tools::ToolSpec;
use serdes_ai::core::ModelRequest;

#[derive(Clone, Copy)]
pub(super) enum InputAccounting {
    Inclusive,
    Anthropic,
}

impl InputAccounting {
    fn observe(self, usage: &LlmUsage, call: Option<u64>) -> Observation {
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

impl Default for RunCache {
    fn default() -> Self {
        Self {
            calls: Some(0),
            measured_calls: Some(0),
            measured_input_tokens: Some(0),
            measured_cached_tokens: Some(0),
            aggregate_valid: true,
            hit_ratio: None,
        }
    }
}

impl RunCache {
    fn record(&mut self, accounting: InputAccounting, usage: &LlmUsage) -> Observation {
        self.calls = self.calls.and_then(|n| n.checked_add(1));
        let observation = accounting.observe(usage, self.calls);
        if let Some((input, read)) = observation
            .total_input_tokens
            .zip(observation.cached_input_tokens)
            .filter(|(input, read)| read <= input)
        {
            self.measured_calls = self.measured_calls.and_then(|n| n.checked_add(1));
            // Each total becomes permanently unknown on overflow. Retain the other
            // exact totals and coverage counts, never a saturated or partial ratio.
            self.measured_input_tokens = self
                .measured_input_tokens
                .and_then(|n| n.checked_add(input));
            self.measured_cached_tokens = self
                .measured_cached_tokens
                .and_then(|n| n.checked_add(read));
        }
        self.aggregate_valid = self.calls.is_some()
            && self.measured_calls.is_some()
            && self.measured_input_tokens.is_some()
            && self.measured_cached_tokens.is_some();
        self.hit_ratio = self
            .measured_input_tokens
            .zip(self.measured_cached_tokens)
            .filter(|(input, _)| self.aggregate_valid && *input != 0)
            .map(|(input, read)| read as f64 / input as f64);
        observation
    }
}

pub(super) struct ObservedBackend {
    inner: Box<dyn ChatBackend>,
    accounting: InputAccounting,
    run: RefCell<RunCache>,
    latest: RefCell<Option<Observation>>,
}

impl ObservedBackend {
    pub(super) fn new(inner: Box<dyn ChatBackend>, accounting: InputAccounting) -> Self {
        Self {
            inner,
            accounting,
            run: RefCell::new(RunCache::default()),
            latest: RefCell::new(None),
        }
    }
}

impl ChatBackend for ObservedBackend {
    fn tool_error_prefix(&self) -> &'static str {
        self.inner.tool_error_prefix()
    }

    fn request<'a>(
        &'a self,
        requests: &'a [ModelRequest],
        tools: &'a [ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move {
            let result = self.inner.request(requests, tools).await?;
            let mut run = self.run.borrow_mut();
            let observation = run.record(self.accounting, &result.usage);
            *self.latest.borrow_mut() = Some(observation);
            Ok(result)
        })
    }

    fn request_calls(&self) -> usize {
        self.inner.request_calls()
    }

    fn cache_observation(&self) -> Option<Snapshot> {
        self.latest.borrow().clone().map(|call| Snapshot {
            call,
            run: self.run.borrow().clone(),
        })
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
                cache_creation_tokens: Some(130),
                cache_read_tokens: Some(50),
                ..Default::default()
            },
        );
        assert_eq!(second.total_input_tokens, Some(200));
        assert_eq!(second.uncached_input_tokens, Some(150));
        assert_eq!(run.hit_ratio, Some(110.0 / 300.0));
        assert_eq!(run.calls, Some(4));
        assert_eq!(run.measured_calls, Some(3));
        let missing = run.record(
            InputAccounting::Anthropic,
            &LlmUsage {
                request_tokens: Some(20),
                cache_read_tokens: Some(50),
                ..Default::default()
            },
        );
        assert_eq!(missing.total_input_tokens, None);
        assert_eq!(run.measured_calls, Some(3));
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
        assert_eq!(run.measured_calls, Some(0));
        assert_eq!(run.hit_ratio, None);
    }

    fn usage(input: u64, read: u64) -> LlmUsage {
        LlmUsage {
            request_tokens: Some(input),
            cache_read_tokens: Some(read),
            ..Default::default()
        }
    }

    #[test]
    fn multicall_input_overflow_preserves_raw_values_and_measured_coverage() {
        let mut run = RunCache::default();
        run.record(InputAccounting::Inclusive, &LlmUsage::default());
        run.record(InputAccounting::Inclusive, &usage(u64::MAX, 0));
        assert!(run.aggregate_valid);
        assert_eq!(run.hit_ratio, Some(0.0));
        let overflow = run.record(InputAccounting::Inclusive, &usage(1, 0));
        assert_eq!(overflow.reported_input_tokens, Some(1));
        assert_eq!(overflow.total_input_tokens, Some(1));
        assert_eq!(overflow.cached_input_tokens, Some(0));
        assert_eq!(run.measured_input_tokens, None);
        assert_eq!(run.measured_cached_tokens, Some(0));
        assert!(!run.aggregate_valid);
        assert_eq!(run.hit_ratio, None);
        run.record(InputAccounting::Inclusive, &LlmUsage::default());
        run.record(InputAccounting::Inclusive, &usage(2, 1));
        assert_eq!(run.calls, Some(5));
        assert_eq!(run.measured_calls, Some(3));
        assert_eq!(run.measured_input_tokens, None);
        assert_eq!(run.measured_cached_tokens, Some(1));
        assert_eq!(run.hit_ratio, None);
        let serialized = serde_json::to_value(&run).unwrap();
        assert!(serialized["measured_input_tokens"].is_null());
        assert_eq!(serialized["aggregate_valid"], false);
    }

    #[test]
    fn multicall_cached_overflow_is_permanently_unmeasurable() {
        for accounting in [InputAccounting::Inclusive, InputAccounting::Anthropic] {
            let mut run = RunCache::default();
            for read in [u64::MAX, 1, 0] {
                let mut usage = usage(read, read);
                if matches!(accounting, InputAccounting::Anthropic) {
                    usage.request_tokens = Some(0);
                    usage.cache_creation_tokens = Some(0);
                }
                let observation = run.record(accounting, &usage);
                assert_eq!(observation.cached_input_tokens, Some(read));
                assert_eq!(observation.total_input_tokens, Some(read));
                assert_eq!(observation.uncached_input_tokens, Some(0));
            }
            assert_eq!(run.calls, Some(3));
            assert_eq!(run.measured_calls, Some(3));
            assert_eq!(run.measured_input_tokens, None);
            assert_eq!(run.measured_cached_tokens, None);
            assert_eq!(run.hit_ratio, None);
            assert!(!run.aggregate_valid);
        }
    }

    #[test]
    fn partial_invalid_and_zero_calls_retain_exact_measured_subset() {
        let mut run = RunCache::default();
        run.record(InputAccounting::Inclusive, &usage(0, 0));
        assert!(run.aggregate_valid);
        assert_eq!(run.hit_ratio, None);
        run.record(InputAccounting::Inclusive, &usage(100, 25));
        run.record(
            InputAccounting::Inclusive,
            &LlmUsage {
                request_tokens: Some(u64::MAX),
                ..Default::default()
            },
        );
        run.record(InputAccounting::Inclusive, &usage(1, 2));
        run.record(
            InputAccounting::Anthropic,
            &LlmUsage {
                request_tokens: Some(u64::MAX),
                cache_creation_tokens: Some(1),
                cache_read_tokens: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(run.calls, Some(5));
        assert_eq!(run.measured_calls, Some(2));
        assert_eq!(run.measured_input_tokens, Some(100));
        assert_eq!(run.measured_cached_tokens, Some(25));
        assert_eq!(run.hit_ratio, Some(0.25));
        assert!(run.aggregate_valid);
    }

    #[test]
    fn call_and_measured_count_overflow_do_not_fabricate_ordinals_or_ratio() {
        // These bounds cannot be reached in a practical process; seed the exact
        // predecessor to exercise the same checked arithmetic used for every call.
        for measured_overflow in [false, true] {
            let mut run = RunCache {
                calls: Some(u64::MAX),
                measured_calls: Some(if measured_overflow { u64::MAX } else { 0 }),
                ..Default::default()
            };
            let observation = run.record(InputAccounting::Inclusive, &usage(1, 1));
            assert_eq!(observation.call, None);
            assert_eq!(observation.cached_input_tokens, Some(1));
            assert_eq!(run.calls, None);
            let expected_measured = if measured_overflow { None } else { Some(1) };
            assert_eq!(run.measured_calls, expected_measured);
            assert_eq!(run.measured_input_tokens, Some(1));
            assert_eq!(run.measured_cached_tokens, Some(1));
            assert!(!run.aggregate_valid);
            assert_eq!(run.hit_ratio, None);
            run.record(InputAccounting::Inclusive, &usage(1, 0));
            assert_eq!(run.calls, None);
            assert_eq!(run.hit_ratio, None);
        }
    }

    struct BoundaryBackend {
        attempts: std::cell::Cell<usize>,
    }

    impl ChatBackend for BoundaryBackend {
        fn tool_error_prefix(&self) -> &'static str {
            "tool error: "
        }

        fn request<'a>(
            &'a self,
            _requests: &'a [ModelRequest],
            _tools: &'a [ToolSpec],
        ) -> crate::adapter::ModelFuture<'a> {
            Box::pin(async move {
                let call = self.attempts.get() + 1;
                self.attempts.set(call);
                match call {
                    1 => Ok(crate::adapter::LlmResult {
                        thinking: "fixture reasoning".to_string(),
                        usage: LlmUsage {
                            request_tokens: Some(100),
                            cache_read_tokens: Some(60),
                            ..Default::default()
                        },
                        text: "fixture completion".to_string(),
                        calls: Vec::new(),
                        finish_reason: Some(serdes_ai::core::messages::FinishReason::Stop),
                    }),
                    2 => Err("fixture terminal failure".to_string()),
                    _ => std::future::pending().await,
                }
            })
        }

        fn request_calls(&self) -> usize {
            self.attempts.get()
        }
    }

    #[test]
    fn failed_and_cancelled_requests_do_not_add_completion_usage_or_replay() {
        let observed = ObservedBackend::new(
            Box::new(BoundaryBackend {
                attempts: std::cell::Cell::new(0),
            }),
            InputAccounting::Inclusive,
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(observed.tool_error_prefix(), "tool error: ");
        rt.block_on(async {
            let reply = observed.request(&[], &[]).await.unwrap();
            assert_eq!(reply.thinking, "fixture reasoning");
            assert_eq!(reply.usage.cache_read_tokens, Some(60));
            assert_eq!(observed.request_calls(), 1);
            assert_eq!(
                observed.request(&[], &[]).await.unwrap_err(),
                "fixture terminal failure"
            );
            assert_eq!(observed.request_calls(), 2);
            assert!(tokio::time::timeout(
                std::time::Duration::from_millis(10),
                observed.request(&[], &[])
            )
            .await
            .is_err());
        });
        assert_eq!(observed.request_calls(), 3);
        let run = observed.run.borrow();
        assert_eq!(run.calls, Some(1));
        assert_eq!(run.measured_calls, Some(1));
        assert_eq!(run.measured_input_tokens, Some(100));
        assert_eq!(run.measured_cached_tokens, Some(60));
        assert_eq!(run.hit_ratio, Some(0.6));
    }
}
