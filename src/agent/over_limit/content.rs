//! Recovery-only comparison of message content, excluding creation timestamps.
//! This does not replace the complete admission estimator or provider conversion.

use serdes_ai::core::{ModelRequest, ModelRequestPart};

/// Serialize typed fields rather than reparsing JSON or rewriting timestamps. Keep
/// content's own timestamp-shaped fields, IDs, error state and all other metadata.
fn part_content(part: &ModelRequestPart) -> Result<Vec<u8>, serde_json::Error> {
    match part {
        ModelRequestPart::SystemPrompt(p) => {
            serde_json::to_vec(&(p.part_kind(), &p.content, &p.dynamic_ref))
        }
        ModelRequestPart::UserPrompt(p) => serde_json::to_vec(&(p.part_kind(), &p.content)),
        ModelRequestPart::ToolReturn(p) => {
            serde_json::to_vec(&(p.part_kind(), &p.tool_name, &p.content, &p.tool_call_id))
        }
        ModelRequestPart::RetryPrompt(p) => {
            serde_json::to_vec(&("retry-prompt", &p.content, &p.tool_name, &p.tool_call_id))
        }
        ModelRequestPart::BuiltinToolReturn(p) => serde_json::to_vec(&(
            "builtin-tool-return",
            &p.tool_name,
            &p.content,
            &p.tool_call_id,
            &p.id,
            &p.provider_details,
        )),
        ModelRequestPart::ModelResponse(p) => serde_json::to_vec(&(
            "model-response",
            &p.parts,
            &p.model_name,
            &p.finish_reason,
            &p.usage,
            &p.vendor_id,
            &p.vendor_details,
            &p.kind,
        )),
    }
}

pub(super) fn request_content(request: &ModelRequest) -> Result<Vec<u8>, serde_json::Error> {
    let parts = request
        .parts
        .iter()
        .map(part_content)
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_vec(&(&request.kind, parts))
}

fn content_bytes(requests: &[ModelRequest]) -> Result<usize, serde_json::Error> {
    requests.iter().try_fold(0usize, |n, request| {
        request
            .parts
            .iter()
            .try_fold(n.saturating_add(request.kind.len()), |n, part| {
                Ok(n.saturating_add(part_content(part)?.len()))
            })
    })
}

/// Eligibility requires a strict reduction in stable message content, not a
/// shorter clock rendering. Serialization failure cannot authorize a retry.
pub(crate) fn content_shrank(before: &[ModelRequest], after: &[ModelRequest]) -> bool {
    match (content_bytes(before), content_bytes(after)) {
        (Ok(before), Ok(after)) => after < before,
        _ => false,
    }
}

/// Retain unaffected original messages, including their exact creation metadata.
/// Rebuilt history is an ordered subsequence of original messages apart from the
/// genuinely reclaimed assistant narration. Only those changed messages are new.
pub(super) fn preserve_unchanged_prefix(before: &[ModelRequest], after: &mut [ModelRequest]) {
    let mut cursor = 0;
    for request in after {
        let Ok(content) = request_content(request) else {
            continue;
        };
        if let Some(index) = before[cursor..]
            .iter()
            .position(|old| request_content(old).is_ok_and(|old| old == content))
        {
            cursor += index;
            *request = before[cursor].clone();
            cursor += 1;
        }
    }
}
