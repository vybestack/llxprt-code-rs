//! Real SSE arrival times, not inferred from the folded response.
use super::*;
use crate::adapter::{user_request, ChatBackend};
use crate::session::{SessionId, SessionStore};
use crate::test_loopback as loopback;
use std::io::Write as _;
use std::time::Duration;

#[test]
fn delayed_sse_reports_first_body_generation_reasoning_and_exact_usage() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = loopback::spawn(listener, move |listener| {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = tests::read_codex_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        let payload = tests::codex_turn_sse_payload(0)
            .replace("\"input_tokens\":11", "\"input_tokens\":100")
            .replace("\"output_tokens\":7", "\"output_tokens\":10")
            .replace(
                "\"total_tokens\":18",
                "\"total_tokens\":117,\"output_tokens_details\":{\"reasoning_tokens\":7}",
            )
            .replace("\"cached_tokens\":6", "\"cached_tokens\":60");
        let body = payload.split_once("\r\n\r\n").unwrap().1;
        let (first, rest) = body.split_once("\n\n").unwrap();
        write!(stream, "{first}\n\n").unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(90));
        stream.write_all(rest.as_bytes()).unwrap();
    });
    let model = OpenResponsesModel::new("fixture", format!("http://{address}"))
        .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap())
        .codex_http();
    let backend = crate::model_api::cache_observation::ObservedBackend::new(
        Box::new(ResponsesBackend::new(model, ModelSettings::default())),
        crate::model_api::cache_observation::InputAccounting::Inclusive,
    );
    let agent =
        crate::agent::CodingAgent::with_backend(Box::new(backend), root.path().into(), false);
    let store =
        SessionStore::load_at(&SessionId::parse("sse-telemetry").unwrap(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let run = agent.run(&store, &reserved).unwrap();
    server.join().unwrap();
    let telemetry = run.telemetry.unwrap();
    let call = &telemetry.rounds[0];
    assert_eq!(call.input_tokens, Some(100));
    assert_eq!(call.output_tokens, Some(10));
    assert_eq!(call.total_tokens, Some(117)); // provider fact, not 100+10
    assert_eq!(call.reasoning_tokens, Some(7));
    assert_eq!(call.cached_input_tokens, Some(60));
    assert!(call.ttft_ms.unwrap() >= 30);
    assert!(call.gen_ms.unwrap() >= 70);
    assert!(call.ttft_ms.unwrap() < call.wall_ms);
    assert_eq!(call.tok_s, Some(10_000.0 / call.gen_ms.unwrap() as f64));
    assert_eq!(telemetry.total_tokens, Some(117));
    assert_eq!(agent.model_calls(), 1);
    let state = store.snapshot().unwrap();
    assert_eq!(
        serde_json::to_value(state.branches[0].telemetry.as_ref().unwrap()).unwrap(),
        serde_json::to_value(&telemetry).unwrap()
    );
    // A distinct request in this runtime has no stale transport observations.
    let collector = serdes_ai::models::telemetry::Collector::default();
    let direct = ResponsesBackend::new(
        OpenResponsesModel::new("fixture", "bad-url"),
        ModelSettings::default(),
    );
    let requests = [user_request("P")];
    let _ = tests::test_runtime().block_on(collector.scope(direct.request(&requests, &[])));
    assert!(collector.snapshot().first_body_ms.is_none());
}
