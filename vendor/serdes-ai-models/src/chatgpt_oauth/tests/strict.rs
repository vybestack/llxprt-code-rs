use super::super::*;
use super::{invalid_message, ordered_stream, parse_test, push_event, strict_stream, test_limits};
use serdes_ai_core::messages::ToolCallArgs;
use serdes_ai_core::{FinishReason, ModelResponsePart};

#[test]
fn sse_preserves_parts_raw_arguments_metadata_and_discards_reasoning() {
    let response = parse_test(&ordered_stream(), test_limits()).unwrap();
    assert_eq!(response.model_name.as_deref(), Some("GPT-5-Codex"));
    assert_eq!(response.vendor_id.as_deref(), Some("response-1"));
    assert_eq!(response.finish_reason, Some(FinishReason::ToolCall));
    assert_eq!(response.usage.as_ref().unwrap().total_tokens, Some(8));
    assert_eq!(response.parts.len(), 3);
    assert!(
        matches!(&response.parts[0], ModelResponsePart::Text(text) if text.content == "before")
    );
    assert!(
        matches!(&response.parts[1], ModelResponsePart::ToolCall(call)
        if call.tool_call_id.as_deref() == Some("call-0")
            && call.args == ToolCallArgs::String("{malformed".to_string()))
    );
    assert!(matches!(&response.parts[2], ModelResponsePart::Text(text) if text.content == "after"));
    assert!(response
        .parts
        .iter()
        .all(|part| !matches!(part, ModelResponsePart::Thinking(_))));
    assert_eq!(MAX_CODEX_SSE_FRAME_BYTES, 1024 * 1024);
    assert_eq!(MAX_CODEX_SSE_EVENTS, 65_536);
    assert_eq!(MAX_CODEX_TEXT_BYTES, 1024 * 1024);
    assert_eq!(MAX_CODEX_REASONING_SUMMARY_BYTES, 1024 * 1024);
    assert_eq!(MAX_CODEX_ARGUMENT_BYTES_PER_CALL, 512 * 1024);
    assert_eq!(MAX_CODEX_ARGUMENT_BYTES_TOTAL, 1024 * 1024);
    assert_eq!(MAX_CODEX_FUNCTION_CALLS, 16);
}

#[test]
fn sse_rejects_missing_final_newline_and_meaningful_data_after_completion() {
    let mut truncated = strict_stream("ok", None, &[]);
    truncated.truncate(truncated.len() - 2);
    assert_eq!(
        invalid_message(parse_test(&truncated, test_limits()).unwrap_err()),
        MALFORMED_SSE_ERROR
    );

    let mut missing_done = strict_stream("ok", None, &[]);
    let done = "data: [DONE]\r\n\r\n";
    missing_done.truncate(missing_done.len() - done.len());
    assert_eq!(
        invalid_message(parse_test(&missing_done, test_limits()).unwrap_err()),
        TERMINAL_ERROR
    );

    let mut trailing = missing_done;
    push_event(
        &mut trailing,
        serde_json::json!({"type":"response.in_progress","response":{"status":"in_progress"}}),
    );
    assert_eq!(
        invalid_message(parse_test(&trailing, test_limits()).unwrap_err()),
        EVENT_ORDER_ERROR
    );
}

#[test]
fn sse_rejects_failure_statuses_and_malformed_completed_shapes() {
    let mut failed = String::new();
    push_event(
        &mut failed,
        serde_json::json!({"type":"response.created","response":{"status":"in_progress"}}),
    );
    push_event(
        &mut failed,
        serde_json::json!({"type":"response.failed","response":{"status":"failed"}}),
    );
    assert_eq!(
        invalid_message(parse_test(&failed, test_limits()).unwrap_err()),
        TERMINAL_ERROR
    );

    for status in ["failed", "cancelled", "in_progress"] {
        let mut stream = strict_stream("ok", None, &[]);
        let offset = stream.rfind("\"status\":\"completed\"").unwrap();
        let end = offset + "\"status\":\"completed\"".len();
        stream.replace_range(offset..end, &format!("\"status\":\"{status}\""));
        assert_eq!(
            invalid_message(parse_test(&stream, test_limits()).unwrap_err()),
            TERMINAL_ERROR
        );
    }
}

#[test]
fn sse_rejects_empty_duplicate_and_conflicting_call_events() {
    let stream = strict_stream("", None, &["{}"]);
    let empty_id = stream.replace("\"call_id\":\"call-0\"", "\"call_id\":\"\"");
    assert_eq!(
        invalid_message(parse_test(&empty_id, test_limits()).unwrap_err()),
        MALFORMED_EVENT_ERROR
    );

    let mut duplicate_frame = String::new();
    push_event(
        &mut duplicate_frame,
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":"item-0","output_index":1,"arguments":"{}"}),
    );
    let done_type = stream
        .find("\"type\":\"response.function_call_arguments.done\"")
        .unwrap();
    let insert = done_type + stream[done_type..].find("\r\n\r\n").unwrap() + 4;
    let mut duplicate = stream.clone();
    duplicate.insert_str(insert, &duplicate_frame);
    assert_eq!(
        invalid_message(parse_test(&duplicate, test_limits()).unwrap_err()),
        EVENT_ORDER_ERROR
    );

    let conflicting = stream.replacen("\"item_id\":\"item-0\"", "\"item_id\":\"other\"", 1);
    assert_eq!(
        invalid_message(parse_test(&conflicting, test_limits()).unwrap_err()),
        EVENT_ORDER_ERROR
    );
}
