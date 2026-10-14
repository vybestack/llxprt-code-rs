//! Oldest-first reclamation inside the existing recovery compactor.
//! Signed/private continuation blocks are never edited: a completed execution is
//! elided atomically with every partner, or retained byte-for-byte. Durable rounds
//! remain in the session store; this only changes the checkpoint's wire projection.
use serdes_ai::core::{ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart};

fn response(request: &ModelRequest) -> Option<&ModelResponse> {
    match request.parts.as_slice() {
        [ModelRequestPart::ModelResponse(response)] => Some(response),
        _ => None,
    }
}

/// Retain the newest tool execution and everything after it (including final
/// instructions). Without tools, retain the last response. Mixed/unknown provider
/// parts are ineligible rather than guessed at or rewritten.
fn evidence_boundary(requests: &[ModelRequest]) -> Option<usize> {
    let responses = || {
        requests.iter().enumerate().flat_map(|(index, request)| {
            request.parts.iter().filter_map(move |part| {
                if let ModelRequestPart::ModelResponse(r) = part {
                    Some((index, r))
                } else {
                    None
                }
            })
        })
    };
    responses()
        .filter(|(_, r)| r.parts.iter().any(ModelResponsePart::is_tool_call))
        .map(|(index, _)| index)
        .next_back()
        .or_else(|| responses().map(|(index, _)| index).next_back())
}

fn return_partner(requests: &[ModelRequest], id: &str, name: &str) -> Option<usize> {
    let returns = requests
        .iter()
        .enumerate()
        .filter(|(_, request)| {
            request.parts.iter().any(|p| {
                if let ModelRequestPart::ToolReturn(t) = p {
                    t.tool_call_id.as_deref() == Some(id)
                } else {
                    false
                }
            })
        })
        .collect::<Vec<_>>();
    let [(index, request)] = returns.as_slice() else {
        return None;
    };
    let [ModelRequestPart::ToolReturn(t)] = request.parts.as_slice() else {
        return None;
    };
    (t.tool_name == name).then_some(*index)
}

fn partners(requests: &[ModelRequest], index: usize, r: &ModelResponse) -> Option<Vec<usize>> {
    let mut group = vec![index];
    for part in &r.parts {
        match part {
            ModelResponsePart::Text(_) | ModelResponsePart::Thinking(_) => (),
            ModelResponsePart::ToolCall(call) => {
                let id = call.tool_call_id.as_deref()?;
                if id.is_empty() || call_count(requests, id) != 1 {
                    return None;
                }
                let partner = return_partner(requests, id, &call.tool_name)?;
                if partner <= index {
                    return None;
                }
                group.push(partner);
            }
            _ => return None,
        }
    }
    Some(group)
}

fn call_count(requests: &[ModelRequest], id: &str) -> usize {
    requests
        .iter()
        .flat_map(|r| &r.parts)
        .filter_map(|p| {
            if let ModelRequestPart::ModelResponse(r) = p {
                Some(&r.parts)
            } else {
                None
            }
        })
        .flatten()
        .filter(|p| {
            if let ModelResponsePart::ToolCall(c) = p {
                c.tool_call_id.as_deref() == Some(id)
            } else {
                false
            }
        })
        .count()
}

fn eligible(r: &ModelResponse, history: &[crate::session::HistoryTurn]) -> bool {
    let meaningful = r.parts.iter().any(|p| match p {
        ModelResponsePart::Text(t) => !t.content.is_empty(),
        ModelResponsePart::Thinking(_) | ModelResponsePart::ToolCall(_) => true,
        _ => false,
    });
    let summary = r.parts.iter().any(|p| {
        if let ModelResponsePart::Text(t) = p {
            history
                .iter()
                .any(|h| !h.summary.is_empty() && h.summary == t.content)
        } else {
            false
        }
    });
    meaningful && !summary
}

fn assistant_dominated(requests: &[ModelRequest], group: &[usize], r: &ModelResponse) -> bool {
    // Preserve evidence-dominated executions: reclaim assistant framing, not useful
    // tool payloads. Admitted digests are often larger than tiny calls.
    let assistant_bytes = serde_json::to_vec(r).map_or(0, |bytes| bytes.len());
    let evidence_bytes = group.iter().skip(1).fold(0usize, |total, i| {
        total.saturating_add(serde_json::to_vec(&requests[*i]).map_or(usize::MAX, |b| b.len()))
    });
    assistant_bytes > evidence_bytes
}

/// Remove just one eligible oldest execution. Repeated calls terminate because
/// every success removes at least one request; the caller checks the complete guard
/// between removals. Completed semantic summaries are mandatory, not old narration.
pub(super) fn elide_oldest(
    requests: &mut Vec<ModelRequest>,
    history: &[crate::session::HistoryTurn],
) -> bool {
    let Some(boundary) = evidence_boundary(requests) else {
        return false;
    };
    for index in 0..boundary {
        let Some(r) = response(&requests[index]) else {
            continue;
        };
        if !eligible(r, history) {
            continue;
        }
        let Some(mut group) = partners(requests, index, r) else {
            continue;
        };
        if !assistant_dominated(requests, &group, r)
            || group.iter().skip(1).any(|partner| *partner >= boundary)
        {
            continue;
        }
        group.sort_unstable();
        for index in group.into_iter().rev() {
            requests.remove(index);
        }
        return true;
    }
    false
}
