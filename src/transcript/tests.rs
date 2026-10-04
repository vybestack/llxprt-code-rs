use super::*;

#[test]
fn emission_payload_budgets_are_shared_and_scrubbed() {
    let mut used = 0;
    let secret = "reflected-key".to_string();
    let first = bounded(
        &format!("{secret} {}", "界".repeat(100)),
        std::slice::from_ref(&secret),
        &mut used,
        96,
    );
    assert!(!first.contains(&secret));
    assert!(first.contains("[truncated]"));
    assert!(used <= 96);
    let next = bounded("next round", &[], &mut used, 96);
    assert!(first.len() + next.len() <= 96);
    let mut emitter = Emitter::new(vec![Emit::All]);
    emitter.assistant_bytes = 100;
    emitter.args_bytes = 100;
    emitter.output_bytes = 100;
    emitter.reset();
    assert_eq!(
        (
            emitter.assistant_bytes,
            emitter.args_bytes,
            emitter.output_bytes
        ),
        (0, 0, 0)
    );
}

#[test]
fn result_prefix_counts_against_aggregate_cap_without_rewriting_bytes() {
    let mut emitter = Emitter::new(vec![Emit::Results]);
    emitter.output_bytes = crate::limits::MAX_TURN_OUTPUT_BYTES - 6;
    let call = crate::session::ToolCallRecord {
        id: "id".into(),
        name: "read_file".into(),
        args: "{}".into(),
        ok: false,
        refused: false,
        result: "durable must not be emitted".into(),
        result_live: "x".into(),
    };
    assert!(emitter
        .result(1, 1, &call, "Error: ", crate::limits::MAX_TURN_OUTPUT_BYTES)
        .is_err());
}

#[test]
fn adapter_captures_provider_thinking_and_chat_does_not_invent_it() {
    use serdes_ai::core::{ModelResponse, ModelResponsePart};
    let mut response = ModelResponse::new();
    response.add_part(ModelResponsePart::thinking("provided"));
    response.add_part(ModelResponsePart::text("answer"));
    let result = crate::adapter::LlmResult::from_response(&response, &[]);
    assert_eq!(result.thinking, "provided");
    let mut chat = ModelResponse::new();
    chat.add_part(ModelResponsePart::text("answer"));
    assert!(crate::adapter::LlmResult::from_response(&chat, &[])
        .thinking
        .is_empty());
}

#[test]
fn live_results_never_fall_back_to_durable_even_when_empty() {
    let mut emitter = Emitter::new(vec![Emit::Results]);
    let mut call = crate::session::ToolCallRecord {
        id: "first".into(),
        name: "read_file".into(),
        args: "{}".into(),
        ok: true,
        refused: false,
        result: "CTXDIGEST durable-only".into(),
        result_live: "admitted live bytes".into(),
    };
    let first = emitter
        .result_event(1, 1, &call, "tool error: ", 128)
        .unwrap()
        .unwrap();
    assert_eq!(first["result"], "admitted live bytes");
    call.id = "second".into();
    call.result_live.clear();
    let second = emitter
        .result_event(1, 1, &call, "tool error: ", 128)
        .unwrap()
        .unwrap();
    assert_eq!(second["result"], "");
    call.ok = false;
    let failed = emitter
        .result_event(1, 2, &call, "tool error: ", 128)
        .unwrap()
        .unwrap();
    assert_eq!(failed["result"], "tool error: ");
    // Captured events are immutable, in ingress order, independent of later mutation.
    assert_eq!(first["id"], "first");
    assert_eq!(second["id"], "second");
    assert_eq!(first["result"], "admitted live bytes");
    assert_eq!(
        emitter.output_bytes,
        "admitted live bytes".len() + "tool error: ".len()
    );
    emitter.reset();
    assert_eq!(emitter.output_bytes, 0);
}

#[test]
fn resolved_cap_is_enforced_without_partial_result_or_hidden_charge() {
    let mut emitter = Emitter::new(vec![Emit::Results]);
    let call = crate::session::ToolCallRecord {
        id: "id".into(),
        name: "read_file".into(),
        args: "{}".into(),
        ok: false,
        refused: false,
        result: "stored".into(),
        result_live: "界".into(),
    };
    assert!(emitter.result_event(1, 1, &call, "Error: ", 9).is_err());
    assert_eq!(emitter.output_bytes, 0);
    let exact = emitter
        .result_event(1, 1, &call, "Error: ", 10)
        .unwrap()
        .unwrap();
    assert_eq!(exact["result"], "Error: 界");
    assert_eq!(emitter.output_bytes, 10);
    assert!(emitter.result_event(1, 2, &call, "Error: ", 10).is_err());
    assert_eq!(emitter.output_bytes, 10);
    let mut off = Emitter::default();
    assert!(off
        .result_event(1, 1, &call, "Error: ", 0)
        .unwrap()
        .is_none());
    assert_eq!(off.output_bytes, 0);
}
