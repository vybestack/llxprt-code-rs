//! Item identity and completion evidence must agree before a folded call escapes.
mod tool_argument_framing;

use serde_json::{json, Value};
use serdes_ai::core::{ModelRequest, ModelSettings};
use serdes_ai::models::{Model, ModelError, ModelRequestParameters};
use tool_argument_framing::{payload, serve};

fn witness(change: impl FnOnce(&mut Vec<Value>)) -> String {
    let body = payload(&[vec!["{\"path\":", "\".\"}"]], false);
    let mut events: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    change(&mut events);
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn rejected(body: String) {
    let (model, server) = serve(vec![body], false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(model.request(
        &[ModelRequest::default()],
        &ModelSettings::default(),
        &ModelRequestParameters::default(),
    ));
    server.join().unwrap(); // exactly one request, empty replay window, checked writes
    let error = result.expect_err("contradiction released a successful response");
    assert!(matches!(error, ModelError::InvalidResponse(_)));
    assert!(!error.is_retryable());
    assert!(!error.to_string().contains("SECRET"));
}

#[test]
fn wrong_argument_item_id_is_not_admitted_by_output_index() {
    rejected(witness(|events| {
        events[2]["item_id"] = json!("SECRET-other-item")
    }));
}

#[test]
fn contradictory_argument_done_never_replaces_delta_bytes() {
    rejected(witness(|events| events[4]["arguments"] = json!("[]")));
}

#[test]
fn all_tool_completion_evidence_is_bound_to_start_and_delta_bytes() {
    for field in ["id", "call_id", "name", "arguments", "status"] {
        for terminal in [false, true] {
            rejected(witness(|events| {
                let item = if terminal {
                    &mut events[6]["response"]["output"][0]
                } else {
                    &mut events[5]["item"]
                };
                item[field] = json!("SECRET-contradiction");
                // Use a valid typed status to prove semantic (not parsing) rejection.
                if field == "status" {
                    item[field] = json!("in_progress");
                }
            }));
        }
    }
}

#[test]
fn missing_duplicate_or_out_of_order_tool_lifecycle_is_rejected() {
    for case in 0..11 {
        rejected(witness(|events| match case {
            0 => {
                events.remove(1);
            }
            1 => {
                events.remove(4);
            }
            2 => {
                events.remove(5);
            }
            3 => events.insert(2, events[1].clone()),
            4 => events.insert(5, events[4].clone()),
            5 => events.insert(6, events[5].clone()),
            6 => events.insert(5, events[2].clone()),
            7 => events.insert(6, events[2].clone()),
            8 => events[2]["output_index"] = json!(1),
            9 => events[4]["item_id"] = json!("other-item"),
            10 => {
                let mut extra = events[6]["response"]["output"][0].clone();
                extra["id"] = json!("unstarted");
                events[6]["response"]["output"]
                    .as_array_mut()
                    .unwrap()
                    .push(extra);
            }
            _ => unreachable!(),
        }));
    }
}

#[test]
fn done_json_equivalence_does_not_authorize_rewriting_raw_bytes() {
    rejected(witness(|events| {
        events[4]["arguments"] = json!("{ \"path\": \".\" }")
    }));
}

// Streams captured from the live Codex backend (redacted to event shapes).
const LIVE_DELTAS_EMPTY_TERMINAL: &str = include_str!(
    "../vendor/serdes-ai-responses/tests/tool_lifecycle/live_deltas_empty_terminal.sse"
);
const LIVE_DONE_ONLY: &str =
    include_str!("../vendor/serdes-ai-responses/tests/tool_lifecycle/live_done_only_arguments.sse");

fn accepted_arguments(body: &str, fragments: bool) -> Vec<String> {
    let (model, server) = serve(vec![body.to_string()], fragments);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime
        .block_on(model.request(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        ))
        .expect("live wire shape must be accepted");
    server.join().unwrap();
    response
        .tool_call_parts()
        .map(|part| part.args.to_json_string().unwrap())
        .collect()
}

#[test]
fn live_wire_terminal_output_may_omit_streamed_tool_items() {
    for fragments in [false, true] {
        assert_eq!(
            accepted_arguments(LIVE_DELTAS_EMPTY_TERMINAL, fragments),
            ["{\"path\":\".\"}"]
        );
    }
}

#[test]
fn live_wire_arguments_only_in_done_are_used_when_no_deltas_arrived() {
    for fragments in [false, true] {
        assert_eq!(
            accepted_arguments(LIVE_DONE_ONLY, fragments),
            [
                "{\"path\": \".\"}",
                "{\"limit\": 1000, \"max_output_bytes\": 10000, \"offset\": 0, \"path\": \"a.txt\"}"
            ]
        );
    }
}

#[test]
fn live_wire_done_arguments_never_override_arrived_deltas() {
    // Same live stream, but done contradicts the delta bytes that did arrive.
    rejected(LIVE_DELTAS_EMPTY_TERMINAL.replacen(
        "\"arguments\":\"{\\\"path\\\":\\\".\\\"}\",\"item_id\"",
        "\"arguments\":\"[]\",\"item_id\"",
        1,
    ));
}
