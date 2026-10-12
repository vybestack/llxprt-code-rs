//! Selected stateless Codex HTTP SSE mapping through the stock agent and session store.
use llxprt_code_rs::adapter::{ChatBackend, LlmResult};
use llxprt_code_rs::agent::CodingAgent;
use llxprt_code_rs::session::{SessionId, SessionStore};
use serde_json::{json, Value};
use serdes_ai::models::Model as _;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Backend {
    model: serdes_ai_responses::client::OpenResponsesModel,
    calls: AtomicUsize,
}
impl ChatBackend for Backend {
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        _tools: &'a [llxprt_code_rs::tools::ToolSpec],
    ) -> llxprt_code_rs::adapter::ModelFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .model
                .request(requests, &Default::default(), &Default::default())
                .await
                .map_err(llxprt_code_rs::adapter::BackendError::from_model)?;
            Ok(LlmResult::from_response(&response, &[]))
        })
    }
    fn request_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
fn backend(url: &str) -> Backend {
    Backend {
        model: serdes_ai_responses::client::OpenResponsesModel::new("fixture-codex", url)
            .codex_http()
            .with_reasoning(serdes_ai_responses::types::ReasoningSettings {
                effort: None,
                summary: Some(json!("auto")),
            }),
        calls: AtomicUsize::new(0),
    }
}
fn request(listener: &std::net::TcpListener) -> (std::net::TcpStream, Value) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut socket = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    };
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let mut reader = std::io::BufReader::new(&mut socket);
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap();
            }
        }
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    (socket, serde_json::from_slice(&bytes).unwrap())
}
fn stream(index: usize) -> String {
    let tool = matches!(index, 0 | 2 | 3);
    let call = format!("call-{index}");
    let summary = if index == 0 {
        json!([])
    } else {
        json!([{"type":"summary_text","text":format!("codex-readable-{index}"),"annotations":["fixture"]}])
    };
    let reasoning = json!({"type":"reasoning","id":format!("rs-{index}"),"summary":summary,"encrypted_content":format!("codex-encrypted-{index}"),"status":"completed"});
    let item = if tool {
        json!({"type":"function_call","id":format!("fc-{index}"),"call_id":call,"name":"read_file","arguments":"{\"path\":\"input.txt\"}","status":"completed"})
    } else {
        json!({"type":"message","id":format!("msg-{index}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done","annotations":[]}],"status":"completed"})
    };
    let created = json!({"id":format!("resp-{index}"),"object":"response","created_at":1,"status":"in_progress","model":"fixture-codex","output":[]});
    let mut events =
        vec![json!({"type":"response.created","sequence_number":0,"response":created})];
    for (output_index, item) in [reasoning, item].iter().enumerate() {
        let mut start = item.clone();
        if output_index == 0 {
            start["summary"] = json!([]);
            start.as_object_mut().unwrap().remove("encrypted_content");
        } else if tool {
            start["arguments"] = json!("");
            start["status"] = json!("in_progress");
        } else {
            start["content"] = json!([]);
            start["status"] = json!("in_progress");
        }
        events.push(json!({"type":"response.output_item.added","sequence_number":events.len(),"output_index":output_index,"item":start}));
        if output_index == 0 && index != 0 && index != 4 {
            events.push(json!({"type":"response.reasoning_summary_text.delta","sequence_number":events.len(),"item_id":format!("rs-{index}"),"output_index":0,"summary_index":0,"delta":format!("codex-readable-{index}")}));
        }
        if output_index == 1 && !tool {
            events.push(json!({"type":"response.output_text.delta","sequence_number":events.len(),"output_index":1,"item_id":format!("msg-{index}"),"content_index":0,"delta":"done"}));
        }
        if output_index == 1 && tool {
            events.push(json!({"type":"response.function_call_arguments.done","sequence_number":events.len(),"output_index":1,"item_id":format!("fc-{index}"),"arguments":"{\"path\":\"input.txt\"}"}));
        }
        events.push(json!({"type":"response.output_item.done","sequence_number":events.len(),"output_index":output_index,"item":item}));
    }
    // Real selected backend can carry an empty terminal output: completed items
    // must retain their state, not be overwritten by this empty verdict.
    let complete = json!({"id":format!("resp-{index}"),"object":"response","created_at":1,"status":"completed","model":"fixture-codex","output":[]});
    events.push(
        json!({"type":"response.completed","sequence_number":events.len(),"response":complete}),
    );
    events
        .into_iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}

#[test]
fn codex_completed_items_keep_encryption_ids_order_and_phase_across_restart() {
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("LLXPRT_CONFIG_HOME", root.path());
    std::fs::write(root.path().join("input.txt"), "evidence").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for i in 0..5 {
            let (mut socket, body) = request(&listener);
            requests.push(body);
            let sse = stream(i);
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",sse.len()).unwrap();
        }
        requests
    });
    let id = SessionId::parse("codex-state").unwrap();
    for turn in 0..2 {
        let store = SessionStore::load_at(&id, root.path()).unwrap();
        let reserved = store
            .start_request(
                None,
                None,
                if turn == 0 { "first" } else { "second" },
                root.path(),
            )
            .unwrap();
        let agent =
            CodingAgent::with_backend(Box::new(backend(&url)), root.path().to_owned(), false)
                .with_max_tool_calls(Some(if turn == 0 { 1 } else { 2 }));
        let run = agent.run(&store, &reserved).unwrap();
        assert_eq!(run.summary, "done");
        assert_eq!(run.tool_count, if turn == 0 { 1 } else { 2 });
        let state = store.snapshot().unwrap();
        assert_eq!(
            state.branches[turn].rounds.last().unwrap().thinking,
            format!("codex-readable-{}", if turn == 0 { 1 } else { 4 })
        );
        if turn == 0 {
            store.replace_snapshot(&state).unwrap();
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(store.session_dir().join("session.manifest.json")).unwrap(),
            )
            .unwrap();
            std::fs::write(
                store
                    .session_dir()
                    .join(manifest["current"]["snapshot"].as_str().unwrap()),
                "broken",
            )
            .unwrap();
        }
    }
    for (i, r) in server.join().unwrap().iter().enumerate() {
        assert_eq!(r["store"], false);
        assert_eq!(r["include"], json!(["reasoning.encrypted_content"]));
        assert!(r.get("previous_response_id").is_none());
        for prior in 0..i {
            let input = r["input"].as_array().unwrap();
            let pos = input
                .iter()
                .position(|v| v["id"] == format!("rs-{prior}"))
                .unwrap();
            assert_eq!(
                input[pos]["encrypted_content"],
                format!("codex-encrypted-{prior}")
            );
            assert_eq!(
                input[pos]["summary"].as_array().unwrap().len(),
                usize::from(prior != 0)
            );
            assert_eq!(input[pos]["status"], "completed");
            if prior != 0 {
                assert_eq!(input[pos]["summary"][0]["annotations"], json!(["fixture"]));
            }
            if matches!(prior, 0 | 2 | 3) {
                assert_eq!(input[pos + 1]["call_id"], format!("call-{prior}"));
                assert_eq!(input[pos + 2]["type"], "function_call_output");
                assert_eq!(input[pos + 2]["call_id"], format!("call-{prior}"));
            }
        }
        if i >= 2 {
            assert!(r["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["phase"] == "final_answer"));
        }
    }
}
