//! Content-free accounting for a request that remains over the heuristic guard.
//! Uses the admission estimator's serialized parts and framing, not token counts.

use serdes_ai::core::{ModelRequest, ModelRequestPart};

#[derive(Default)]
struct RetainedBytes {
    system: usize,
    prompts: usize,
    results: usize,
    responses: usize,
    framing: usize,
    schemas: usize,
}

impl RetainedBytes {
    fn measure(requests: &[ModelRequest], tools: &[crate::tools::ToolSpec]) -> Self {
        let mut bytes = Self {
            framing: crate::agent::REQUEST_FIXED_OVERHEAD_BYTES,
            schemas: crate::adapter::estimate_tool_schema_bytes(tools),
            ..Self::default()
        };
        let quoted = crate::context_kernel::security::quote_requests(requests);
        for request in &quoted {
            bytes.framing = bytes
                .framing
                .saturating_add(crate::agent::PER_REQUEST_OVERHEAD_BYTES);
            for part in &request.parts {
                bytes.framing = bytes
                    .framing
                    .saturating_add(crate::agent::PER_PART_OVERHEAD_BYTES);
                // Match estimate_request_bytes, including its serialization-failure handling.
                let size = serde_json::to_vec(part).map(|v| v.len()).unwrap_or(0);
                let category = match part {
                    ModelRequestPart::SystemPrompt(_) => &mut bytes.system,
                    ModelRequestPart::UserPrompt(_) | ModelRequestPart::RetryPrompt(_) => {
                        &mut bytes.prompts
                    }
                    ModelRequestPart::ToolReturn(_) | ModelRequestPart::BuiltinToolReturn(_) => {
                        &mut bytes.results
                    }
                    ModelRequestPart::ModelResponse(_) => &mut bytes.responses,
                };
                *category = category.saturating_add(size);
            }
        }
        bytes
    }

    fn describe(&self) -> String {
        format!(
            "retained estimated bytes: system/instructions={}, prompts/notices={}, tool results={}, assistant/call framing={}, fixed/request/part overhead={}, tool schemas={}; prompts, instructions, call/result pairing and latest tool evidence are retained",
            self.system, self.prompts, self.results, self.responses, self.framing, self.schemas,
        )
    }
}

/// Both sizes include the same tool schemas and admission framing. Report excess
/// separately from totals; never disclose prompt, tool result, or provider content.
pub(super) fn over_limit_message(
    original: usize,
    requests: &[ModelRequest],
    tools: &[crate::tools::ToolSpec],
    context_limit: Option<u64>,
) -> String {
    let after = super::request_total_bytes(requests, tools);
    let budget = crate::agent::materialization_budget(context_limit);
    let reduction = original.saturating_sub(after);
    let reason = if after < original {
        "compaction reduced the request but retained content still exceeds the guard"
    } else {
        "compaction did not shrink the request: no eligible history or observed tool results could be removed"
    };
    let retained = RetainedBytes::measure(requests, tools).describe();
    format!(
        "estimated request would be {original} bytes ({} bytes over the {budget}-byte heuristic guard); after one compaction attempt, still {after} bytes ({} bytes over guard); reduced by {reduction} bytes; {reason}; {retained}; no request sent",
        original.saturating_sub(budget), after.saturating_sub(budget),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_categories_sum_to_complete_admission_estimate() {
        let requests = vec![
            crate::adapter::system_request("mandatory 界 instructions"),
            crate::adapter::user_request("prompt \"escaping\""),
            crate::adapter::tool_return_request("read_file", "id", true, "result 界"),
        ];
        let tools = crate::tools::tool_specs(true);
        let b = RetainedBytes::measure(&requests, &tools);
        assert_eq!(
            b.system + b.prompts + b.results + b.responses + b.framing + b.schemas,
            super::super::request_total_bytes(&requests, &tools)
        );
        assert!(b.system > 0);
        assert!(b.prompts > 0);
        assert!(b.results > 0);
        assert!(b.schemas > 0);
    }

    #[test]
    fn observed_issue_values_report_2362_excess_not_602362_excess() {
        let message = over_limit_message(602_362, &[], &[], Some(200_000));
        assert!(message.contains("estimated request would be 602362 bytes (2362 bytes over the 600000-byte heuristic guard)"));
        assert!(!message.contains("602362 bytes over"));
    }
}
