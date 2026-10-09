//! Shared well-formed tool turn; each completion independently corroborates raw bytes.
use serde_json::json;
use serdes_ai_responses::types::{
    CreateResponseRequest, ResponseObject, ResponseStatus, StreamEvent,
};

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
                response.output.clear();
            }
        }
        None => {}
        _ => panic!("unknown test contradiction"),
    }
    events
}
