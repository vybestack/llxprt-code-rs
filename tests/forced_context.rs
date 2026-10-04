//! F2 registered native tools with a real HTTP explicit-context verdict below guard.
//! Test-only public backend uses the existing Responses HTTP/typed error mapping;
//! no credentials, profile switches, or production transport changes.
use llxprt_code_rs::adapter::{ChatBackend, LlmResult, ModelFuture};
use llxprt_code_rs::agent::CodingAgent;
use llxprt_code_rs::session::{SessionId, SessionStore};
use llxprt_code_rs::tools::ToolSpec;
use serde_json::{json, Value};
use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai_responses::client::OpenResponsesModel;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

struct Backend {
    endpoint: String,
    calls: std::sync::atomic::AtomicUsize,
}
impl ChatBackend for Backend {
    fn request<'a>(
        &'a self,
        requests: &'a [ModelRequest],
        tools: &'a [ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let params = llxprt_code_rs::model::SerdeAiParams {
                tools: Arc::new(
                    tools
                        .iter()
                        .map(llxprt_code_rs::adapter::schema_for)
                        .collect(),
                ),
            };
            // Fresh public model deliberately sends the full fixture history; this
            // is a captured stateless test backend, not production chaining policy.
            let response = OpenResponsesModel::new("fixture", &self.endpoint)
                .codex_http()
                .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap())
                .request(
                    requests,
                    &serdes_ai::ModelSettings::default(),
                    &params.to_model_request_parameters(),
                )
                .await
                .map_err(|error| {
                    llxprt_code_rs::transport::context_length_400(&error)
                        .unwrap_or(error)
                        .to_string()
                })?;
            Ok(LlmResult::from_response(&response, &[]))
        })
    }
    fn request_calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn response(index: usize, reject_twice: bool) -> (u16, Value) {
    if index == 5 || (index == 6 && reject_twice) {
        return (
            400,
            json!({"error":{"code":"context_length_exceeded","message":"maximum context length"}}),
        );
    }
    let output = if index <= 3 {
        vec![
            json!({"type":"function_call","id":format!("fc{index}"),"call_id":format!("r{index}"),"name":"read_file",
            "arguments":"{\"path\":\"evidence.txt\",\"max_output_bytes\":16000}","status":"completed"}),
        ]
    } else if index == 4 {
        vec![]
    } else {
        vec![
            json!({"type":"message","id":"final","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":"Natural final summary","annotations":[]}]}),
        ]
    };
    (
        200,
        json!({"id":format!("resp{index}"),"object":"response","created_at":1,"model":"fixture","status":"completed","output":output}),
    )
}

fn server(listener: TcpListener, captured: Arc<Mutex<Vec<Value>>>, reject_twice: bool) {
    for index in 1..=6 {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
            assert!(header.len() <= 65536);
        }
        let header = String::from_utf8(header).unwrap();
        let length: usize = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 900000);
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        captured
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&body).unwrap());
        let (status, body) = response(index, reject_twice);
        let body = if status == 200 {
            let mut events = String::new();
            for (output_index, item) in body["output"].as_array().unwrap().iter().enumerate() {
                events.push_str(&format!("data: {}\n\n",json!({"type":"response.output_item.added","output_index":output_index,"item":item,"sequence_number":output_index+1})));
                let delta = if item["type"] == "function_call" {
                    json!({"type":"response.function_call_arguments.delta","output_index":output_index,"item_id":item["id"],"delta":item["arguments"],"sequence_number":2})
                } else {
                    json!({"type":"response.output_text.delta","output_index":output_index,"content_index":0,"item_id":item["id"],"delta":item["content"][0]["text"],"sequence_number":2})
                };
                events.push_str(&format!("data: {delta}\n\n"));
            }
            events.push_str(&format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":body,"sequence_number":10})
            ));
            events
        } else {
            body.to_string()
        };
        write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }
}

fn run(reject_twice: bool) {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    let store =
        SessionStore::load_at(&SessionId::parse("forced-loopback").unwrap(), work.path()).unwrap();
    let reserved = store
        .start_request(None, None, "complete task", work.path())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let recorded = captured.clone();
    let thread = std::thread::spawn(move || server(listener, recorded, reject_twice));
    let agent = CodingAgent::with_backend(
        Box::new(Backend {
            endpoint,
            calls: Default::default(),
        }),
        work.path().into(),
        false,
    )
    .with_turn_time(Some(std::time::Duration::from_secs(20)));
    let result = agent.run(&store, &reserved);
    assert_eq!(agent.model_calls(), 6, "run result: {result:?}");
    if reject_twice {
        let error = result.unwrap_err();
        assert_eq!(error.key, "context-limit");
        assert!(error.message.contains("retry after compaction"));
    } else {
        let result = result.unwrap();
        assert_eq!(result.summary, "Natural final summary");
        assert_eq!(result.tool_count, 3);
        assert!(!result.replayed);
    }
    thread.join().unwrap();
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 6);
    let old = &captured[4];
    let new = &captured[5];
    let before = old.to_string().len();
    let after = new.to_string().len();
    println!("forced provider loopback wire bytes: {before} -> {after}");
    assert!(before < 900000);
    assert!(after < before - 20000);
    let old = old["input"].as_array().unwrap();
    let new = new["input"].as_array().unwrap();
    assert_eq!(old.len(), new.len());
    assert_eq!(old.last(), new.last());
    assert!(new
        .last()
        .unwrap()
        .to_string()
        .contains("final plain-text summary"));
    for (a, b) in old.iter().zip(new) {
        if a["type"] != "function_call_output" || a["call_id"] == "r3" {
            assert_eq!(a, b);
        }
    }
    let results: Vec<_> = new
        .iter()
        .filter(|m| m["type"] == "function_call_output")
        .collect();
    assert_eq!(results.len(), 3);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result["call_id"], format!("r{}", index + 1));
    }
    assert!(results[0]["output"]
        .as_str()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(results[1]["output"]
        .as_str()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(results[2]["output"]
        .as_str()
        .unwrap()
        .contains("evidence line"));
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.branches[0].lifecycle,
        if reject_twice {
            llxprt_code_rs::session::Lifecycle::Failed
        } else {
            llxprt_code_rs::session::Lifecycle::Completed
        }
    );
    assert!(state.branches[0].rounds[0].calls[0]
        .result
        .contains("CTXDIGEST v1"));
    assert!(state.branches[0]
        .rounds
        .iter()
        .flat_map(|r| &r.calls)
        .all(|c| c.result_live.is_empty()));
}

#[test]
fn explicit_summary_rejection_below_guard_recovers_registered_results_once() {
    run(false);
}

#[test]
fn second_explicit_summary_rejection_never_sends_a_seventh_call() {
    run(true);
}
