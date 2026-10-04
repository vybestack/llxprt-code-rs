use serde_json::{json, Value};
use serdes_ai_responses::types::{
    CreateResponseRequest, ResponseObject, ResponseStatus, StreamEvent,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;
fn event(value: Value) -> StreamEvent {
    serde_json::from_value(value).expect("payload must satisfy the current owning wire type")
}
fn call(index: usize, done: bool, args: &str) -> Value {
    json!({"type":"function_call","id":format!("item-{index}"),"call_id":format!("call-{index}"),"name":"list_directory","arguments":args,"status":if done {"completed"} else {"in_progress"}})
}
pub fn payload(deltas: &[Vec<&str>], reasoning: bool) -> String {
    let request: CreateResponseRequest =
        serde_json::from_value(json!({"model":"fixture","input":[]})).unwrap();
    let mut response = ResponseObject::in_progress("response", 1, "fixture", &request);
    let mut events = vec![StreamEvent::ResponseCreated {
        sequence_number: 0,
        response: response.clone(),
    }];
    for index in 0..deltas.len() {
        events.push(event(json!({"type":"response.output_item.added","sequence_number":1,"output_index":index,"item":call(index,false,"")})));
    }
    let ri = deltas.len();
    if reasoning {
        events.push(event(json!({"type":"response.output_item.added","sequence_number":2,"output_index":ri,"item":{"type":"reasoning","id":"reason","status":"in_progress","summary":[]}})));
        events.push(event(json!({"type":"response.reasoning_summary_part.added","sequence_number":3,"output_index":ri,"item_id":"reason","summary_index":0,"part":{"type":"summary_text","text":""}})));
        events.push(event(json!({"type":"response.reasoning_summary_text.delta","sequence_number":4,"output_index":ri,"item_id":"reason","summary_index":0,"delta":"framing é"})));
    }
    // Ordered starts, interleaved argument fragments, then per-call done boundaries.
    for n in 0..deltas.iter().map(Vec::len).max().unwrap_or(0) {
        for (index, fragments) in deltas.iter().enumerate() {
            if let Some(delta) = fragments.get(n) {
                events.push(event(json!({"type":"response.function_call_arguments.delta","sequence_number":5,"output_index":index,"item_id":format!("item-{index}"),"delta":delta})));
            }
        }
    }
    let mut output = Vec::new();
    for (index, fragments) in deltas.iter().enumerate() {
        let args = fragments.concat();
        let item = call(index, true, &args);
        events.push(event(json!({"type":"response.function_call_arguments.done","sequence_number":6,"output_index":index,"item_id":format!("item-{index}"),"arguments":args})));
        events.push(event(json!({"type":"response.output_item.done","sequence_number":7,"output_index":index,"item":item})));
        output.push(item);
    }
    if reasoning {
        let part = json!({"type":"summary_text","text":"framing é"});
        events.push(event(json!({"type":"response.reasoning_summary_text.done","sequence_number":8,"output_index":ri,"item_id":"reason","summary_index":0,"text":"framing é"})));
        events.push(event(json!({"type":"response.reasoning_summary_part.done","sequence_number":9,"output_index":ri,"item_id":"reason","summary_index":0,"part":part})));
        let item = json!({"type":"reasoning","id":"reason","status":"completed","summary":[part]});
        events.push(event(json!({"type":"response.output_item.done","sequence_number":10,"output_index":ri,"item":item})));
        output.push(item);
    }
    response.status = ResponseStatus::Completed;
    response.usage=Some(serde_json::from_value(json!({"input_tokens":10,"output_tokens":3,"total_tokens":13,"input_tokens_details":{"cached_tokens":5}})).unwrap());
    response.output = output
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    events.push(StreamEvent::ResponseCompleted {
        sequence_number: 11,
        response,
    });
    events
        .into_iter()
        .enumerate()
        .map(|(sequence, event)| {
            let mut value = serde_json::to_value(event).unwrap();
            value["sequence_number"] = json!(sequence);
            format!("data: {value}\r\n\r\n")
        })
        .collect()
}
fn read_request(socket: &mut std::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let n = socket.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(start) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..start]).to_lowercase();
            let len: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= start + 4 + len {
                return serde_json::from_slice(&bytes[start + 4..start + 4 + len]).unwrap();
            }
        }
        assert!(bytes.len() < 2 * 1024 * 1024);
    }
}
pub fn serve(
    bodies: Vec<String>,
    fragments: bool,
) -> (
    serdes_ai_responses::client::OpenResponsesModel,
    std::thread::JoinHandle<()>,
) {
    use serdes_ai_responses::client::OpenResponsesModel;
    let turns = bodies.len();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in bodies {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            requests.push(read_request(&mut socket));
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            if fragments {
                for chunk in body.as_bytes().chunks(1) {
                    socket.write_all(chunk).unwrap();
                }
            } else {
                socket.write_all(body.as_bytes()).unwrap();
            }
        }
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
            "request replay"
        );
        assert_eq!(requests.len(), turns);
        for request in requests {
            assert_eq!(request["stream"], true);
            assert_eq!(request["store"], false);
            for key in ["previous_response_id", "max_output_tokens"] {
                assert!(request.get(key).is_none());
            }
        }
    });
    let model = OpenResponsesModel::new("fixture", format!("http://{address}/responses"))
        .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap())
        .codex_http();
    (model, server)
}
