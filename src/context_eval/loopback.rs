//! Controlled loopback provider for context evals (#37).
//!
//! The server is the only network endpoint a scenario may talk to. It observes every
//! complete serialized request (size, tool names, stream mode) and answers from a
//! deterministic script: one tool call per scripted round against that round's bulk file,
//! then a final assistant message carrying the scenario's exact final marker. Streaming
//! and non-streaming OpenAI Chat shapes are both answered.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// One complete serialized provider request the loopback observed.
#[derive(Clone, Debug)]
pub struct ObservedRequest {
    pub index: usize,
    pub body_bytes: usize,
    pub tool_names: Vec<String>,
    pub streamed: bool,
}

/// Shared observation of the scripted provider.
#[derive(Clone, Debug, Default)]
pub struct Observations {
    pub requests: Vec<ObservedRequest>,
    /// Scripted tool calls actually handed to the runner.
    pub tool_calls_issued: usize,
    /// Final marker response the runner actually received.
    pub final_response_issued: bool,
}

/// Running loopback provider bound to 127.0.0.1.
pub struct Loopback {
    addr: SocketAddr,
    shared: Arc<Mutex<Observations>>,
    stopped: Arc<AtomicBool>,
    bulk: Arc<Mutex<Vec<PathBuf>>>,
    handle: Option<JoinHandle<()>>,
}

/// Script for one provider turn.
struct Turn<'a> {
    index: usize,
    bulk: &'a [PathBuf],
    marker: &'a str,
    block_bytes: usize,
    tool_names: &'a [String],
}

impl Loopback {
    /// Start the scripted provider. `bulk` holds one expanded file per scripted round.
    pub fn start(rounds: usize, bulk: Vec<PathBuf>, marker: &str, block_bytes: usize) -> Loopback {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("loopback addr");
        let shared = Arc::new(Mutex::new(Observations::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let bulk_slot = Arc::new(Mutex::new(Vec::new()));
        let marker = marker.to_string();
        let state = shared.clone();
        let halt = stopped.clone();
        let handle = std::thread::spawn(move || {
            // Non-blocking accept plus a flag poll: `stop()` sets the flag and joins, so a
            // parked accept() that never observes the flag would hang the whole drive.
            listener
                .set_nonblocking(true)
                .expect("loopback listener nonblocking");
            let budget = rounds + 4;
            let mut served = 0usize;
            while served < budget && !halt.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let bulk = bulk_slot.lock().unwrap_or_else(|p| p.into_inner()).clone();
                        serve(stream, served, &bulk, &marker, block_bytes, &state);
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Loopback {
            addr,
            shared,
            stopped,
            handle: Some(handle),
        }
    }

    /// Hand the scripted rounds their bulk files after the workspace exists.
    ///
    /// The port must be bound before the profile is generated, but the bulk fixtures only
    /// exist once the workspace is prepared, so the script is filled in afterwards.
    pub fn set_bulk(&self, bulk: Vec<PathBuf>) {
        *self.bulk.lock().unwrap_or_else(|p| p.into_inner()) = bulk;
    }

    /// Base URL the generated profile points at.
    pub fn base_url(&self) -> String {
        let addr = self.addr;
        format!("http://{addr}/v1")
    }

    /// Snapshot the observations so far.
    pub fn snapshot(&self) -> Observations {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Stop accepting connections and release the thread.
    pub fn stop(mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    index: usize,
    bulk: &[PathBuf],
    marker: &str,
    block_bytes: usize,
    state: &Arc<Mutex<Observations>>,
) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let Some(length) = read_request_length(&mut reader) else {
        let _ = write_response(&mut writer, false, error_body());
        return;
    };
    if length == 0 || length > 64 * 1024 * 1024 {
        let _ = write_response(&mut writer, false, error_body());
        return;
    }
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let tool_names = tool_names(&parsed);
    let streamed = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    record(state, index, length, &tool_names, streamed);
    let turn = Turn {
        index,
        bulk,
        marker,
        block_bytes,
        tool_names: &tool_names,
    };
    respond(&mut writer, streamed, &turn, state);
}

fn read_request_length(reader: &mut BufReader<TcpStream>) -> Option<usize> {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return Some(length);
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
}

fn record(
    state: &Arc<Mutex<Observations>>,
    index: usize,
    length: usize,
    tool_names: &[String],
    streamed: bool,
) {
    let mut obs = state.lock().unwrap_or_else(|p| p.into_inner());
    obs.requests.push(ObservedRequest {
        index,
        body_bytes: length,
        tool_names: tool_names.to_vec(),
        streamed,
    });
}

fn respond(
    writer: &mut TcpStream,
    streamed: bool,
    turn: &Turn<'_>,
    state: &Arc<Mutex<Observations>>,
) {
    let tool = readable_tool(turn.tool_names);
    if turn.index < turn.bulk.len() {
        if let Some((name, arg_key)) = tool {
            // The CLI's file tools open paths relative to --cwd and reject absolute
            // paths, so bulk fixtures live inside the workspace and are addressed by
            // their file name only.
            let path = turn.bulk[turn.index % turn.bulk.len()]
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let args = json!({ arg_key.clone(): path }).to_string();
            let message = json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{ "id": format!("call_{}", turn.index), "type": "function",
                    "function": { "name": name, "arguments": args } }],
            });
            bump(state, |o| o.tool_calls_issued += 1);
            emit(writer, streamed, &choice_json(message, "tool_calls"), true);
            return;
        }
    }
    let last = turn.index + 1 >= turn.bulk.len() || tool.is_none();
    let text = if last {
        format!("CTXEVAL final: {}", turn.marker)
    } else {
        format!(
            "CTXEVAL block {}: {}",
            turn.index,
            "x".repeat(turn.block_bytes.min(4096))
        )
    };
    let message = json!({ "role": "assistant", "content": text });
    if last {
        bump(state, |o| o.final_response_issued = true);
    }
    emit(writer, streamed, &choice_json(message, "stop"), false);
}

fn bump(state: &Arc<Mutex<Observations>>, f: impl FnOnce(&mut Observations)) {
    let mut obs = state.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut obs);
}

fn choice_json(message: Value, finish: &str) -> Value {
    json!({ "id": "ctxeval", "object": "chat.completion", "created": 0,
        "model": "ctxeval-fixture",
        "choices": [{ "index": 0, "finish_reason": finish, "message": message }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 } })
}

fn tool_names(parsed: &Value) -> Vec<String> {
    parsed
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pick the first tool that plausibly reads a file, plus the argument key its schema wants.
fn readable_tool(tool_names: &[String]) -> Option<(String, String)> {
    let name = tool_names
        .iter()
        .find(|n| n.contains("read"))
        .or_else(|| tool_names.first())?
        .clone();
    Some((name, "path".to_string()))
}

fn error_body() -> String {
    json!({ "error": { "message": "ctxeval loopback rejected the request body",
        "type": "invalid_request_error" } })
    .to_string()
}

fn write_response(stream: &mut TcpStream, streamed: bool, body: String) -> std::io::Result<()> {
    let ctype = if streamed {
        "text/event-stream"
    } else {
        "application/json"
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn emit(stream: &mut TcpStream, streamed: bool, payload: &Value, tool_call: bool) {
    if !streamed {
        let _ = write_response(stream, false, payload.to_string());
        return;
    }
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    let message = payload["choices"][0]["message"].clone();
    let delta = if tool_call {
        json!({ "tool_calls": message["tool_calls"] })
    } else {
        json!({ "content": message["content"] })
    };
    let chunk = json!({ "id": "ctxeval", "object": "chat.completion.chunk", "created": 0,
        "model": "ctxeval-fixture", "choices": [{ "index": 0, "delta": delta }] });
    write_chunk(stream, format!("data: {chunk}\n\n").as_bytes());
    write_chunk(stream, b"data: [DONE]\n\n");
    write_chunk(stream, b"");
    let _ = stream.flush();
}

fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) {
    let _ = write!(stream, "{:x}\r\n", bytes.len());
    let _ = stream.write_all(bytes);
    let _ = stream.write_all(b"\r\n");
}
