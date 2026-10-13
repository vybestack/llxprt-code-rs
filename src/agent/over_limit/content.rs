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

/// Locate this turn's prompt in the actual resumed request projection.
pub(super) fn current_prompt_start(requests: &[ModelRequest], prompt: &str) -> usize {
    requests
        .iter()
        .rposition(|request| {
            request.parts.iter().any(|part| {
                if let ModelRequestPart::UserPrompt(p) = part {
                    p.content.as_text() == Some(prompt)
                } else {
                    false
                }
            })
        })
        .map_or(0, |index| index + 1)
}

/// The existing compactor's narration reduction, applied to the exact prefix
/// rather than reconstructed messages. Call/return pairing and metadata stay put.
pub(super) fn compact_narration(
    requests: &mut [ModelRequest],
    history: &[crate::session::HistoryTurn],
) {
    for part in requests.iter_mut().flat_map(|r| &mut r.parts) {
        let ModelRequestPart::ModelResponse(response) = part else {
            continue;
        };
        for piece in &mut response.parts {
            let serdes_ai::core::ModelResponsePart::Text(text) = piece else {
                continue;
            };
            if history.iter().any(|turn| {
                !turn.summary.is_empty()
                    && turn.rounds.iter().any(|round| {
                        round.continuation.is_none()
                            && round.assistant != turn.summary
                            && round.assistant == text.content
                    })
            }) {
                text.content.clear();
            }
        }
    }
}

/// Put a compact, stable summary at the head only when crossing a checkpoint.
/// All subsequent calls inherit it verbatim through the persisted projection.
pub(super) fn checkpoint_head(
    requests: &mut Vec<ModelRequest>,
    history: &[crate::session::HistoryTurn],
) {
    requests.retain(|request| {
        !request.parts.iter().any(|part| {
            if let ModelRequestPart::SystemPrompt(prompt) = part {
                prompt.dynamic_ref.as_deref() == Some("context-checkpoint")
            } else {
                false
            }
        })
    });
    let summary = history
        .last()
        .map(|turn| turn.summary.as_str())
        .unwrap_or("");
    let mut text = String::from("Context checkpoint: older payloads are represented by admitted digests; preserve the latest evidence and tool pairing.");
    if !summary.is_empty() {
        text.push('\n');
        text.push_str(summary);
    }
    let head = ModelRequest {
        parts: vec![ModelRequestPart::SystemPrompt(
            serdes_ai::core::SystemPromptPart::new(text).with_dynamic_ref("context-checkpoint"),
        )],
        ..Default::default()
    };
    // Keep the immutable agent shell and initial user prompt intact. The
    // checkpoint summary precedes the retained execution evidence.
    requests.insert(2.min(requests.len()), head);
}
