use super::*;

#[test]
fn emission_payload_budgets_are_shared_and_scrubbed() {
    let mut used = 0;
    let secret = "reflected-key".to_string();
    let first = bounded(
        &format!("{secret} {}", "界".repeat(100)),
        &[secret.clone()],
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
        result: "x".into(),
    };
    assert!(emitter.result(1, 1, &call, "Error: ").is_err());
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
