//! Shared well-formed tool turn; each completion independently corroborates raw bytes.
//!
//! Also holds streams captured from the live Codex Responses backend (ids, the
//! prompt and tool schemas redacted; event order and field shapes untouched):
//! - `live_deltas_empty_terminal.sse`: arguments stream as deltas and the terminal
//!   `response.completed` carries `"output": []`.
//! - `live_done_only_arguments.sse`: no argument deltas at all; the arguments are
//!   only in `function_call_arguments.done` / `output_item.done`, and the terminal
//!   output lists the calls.
use serde_json::json;
use serdes_ai_responses::types::{
    CreateResponseRequest, OutputItem, ResponseObject, ResponseStatus, StreamEvent,
};

const LIVE_DELTAS_EMPTY_TERMINAL: &str = include_str!("live_deltas_empty_terminal.sse");
const LIVE_DONE_ONLY: &str = include_str!("live_done_only_arguments.sse");

/// Raw SSE bytes of a captured live turn.
pub fn live(done_only: bool) -> &'static str {
    if done_only {
        LIVE_DONE_ONLY
    } else {
        LIVE_DELTAS_EMPTY_TERMINAL
    }
}

/// The exact argument bytes each captured call carries, in output order.
pub fn live_arguments(done_only: bool) -> Vec<&'static str> {
    if done_only {
        vec![
            "{\"path\": \".\"}",
            "{\"limit\": 1000, \"max_output_bytes\": 10000, \"offset\": 0, \"path\": \"a.txt\"}",
        ]
    } else {
        vec!["{\"path\":\".\"}"]
    }
}

/// The captured turn as typed events, with one real lifecycle contradiction.
pub fn live_events(done_only: bool, contradiction: Option<&str>) -> Vec<StreamEvent> {
    let mut events: Vec<StreamEvent> = live(done_only)
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).expect("captured event parses"))
        .collect();
    let Some(case) = contradiction else {
        return events;
    };
    let mut applied = false;
    for event in &mut events {
        match (case, event) {
            ("done", StreamEvent::FunctionCallArgumentsDone { arguments, .. }) if !applied => {
                *arguments = "[]".into();
                applied = true;
            }
            (
                "item",
                StreamEvent::OutputItemDone {
                    item: OutputItem::FunctionCall { arguments, .. },
                    ..
                },
            ) if !applied => {
                *arguments = "[]".into();
                applied = true;
            }
            ("terminal", StreamEvent::ResponseCompleted { response, .. }) => {
                if let Some(OutputItem::FunctionCall { arguments, .. }) =
                    response.output.first_mut()
                {
                    *arguments = "[]".into();
                    applied = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        applied,
        "contradiction {case} not applicable to this capture"
    );
    events
}

pub fn events(contradiction: Option<&str>) -> Vec<StreamEvent> {
    let args = "{} \n";
    let start = json!({"type":"function_call","id":"item-0","call_id":"call-0","name":"list_directory","arguments":"","status":"in_progress"});
    let mut done = start.clone();
    done["arguments"] = json!(args);
    done["status"] = json!("completed");
    let request: CreateResponseRequest =
        serde_json::from_value(json!({"model":"fixture","input":[]})).unwrap();
    let mut response = ResponseObject::in_progress("r", 1, "fixture", &request);
    response.status = ResponseStatus::Completed;
    response.output = vec![serde_json::from_value(done.clone()).unwrap()];
    let mut events = vec![
        StreamEvent::OutputItemAdded {
            sequence_number: 0,
            output_index: 0,
            item: serde_json::from_value(start).unwrap(),
        },
        StreamEvent::FunctionCallArgumentsDelta {
            sequence_number: 1,
            output_index: 0,
            item_id: "item-0".into(),
            delta: "{}".into(),
        },
        StreamEvent::FunctionCallArgumentsDelta {
            sequence_number: 2,
            output_index: 0,
            item_id: "item-0".into(),
            delta: " \n".into(),
        },
        StreamEvent::FunctionCallArgumentsDone {
            sequence_number: 3,
            output_index: 0,
            item_id: "item-0".into(),
            arguments: args.into(),
        },
        StreamEvent::OutputItemDone {
            sequence_number: 4,
            output_index: 0,
            item: serde_json::from_value(done).unwrap(),
        },
        StreamEvent::ResponseCompleted {
            sequence_number: 5,
            response,
        },
    ];
    match contradiction {
        Some("item") => {
            if let StreamEvent::FunctionCallArgumentsDelta { item_id, .. } = &mut events[1] {
                *item_id = "SECRET-other-item".into();
            }
        }
        Some("done") => {
            if let StreamEvent::FunctionCallArgumentsDone { arguments, .. } = &mut events[3] {
                *arguments = "[]".into();
            }
        }
        Some("terminal") => {
            if let StreamEvent::ResponseCompleted { response, .. } = &mut events[5] {
                if let OutputItem::FunctionCall { arguments, .. } = &mut response.output[0] {
                    *arguments = "[]".into();
                }
            }
        }
        None => {}
        _ => panic!("unknown test contradiction"),
    }
    events
}
