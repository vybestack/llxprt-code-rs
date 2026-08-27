use super::super::*;
use super::{
    finish_stream, generated_ascii, invalid_message, json_event_count, parse_adversarial,
    push_event, push_message, push_reasoning_named, start_stream, strict_stream,
};
use serdes_ai_core::messages::ToolCallArgs;
use serdes_ai_core::ModelResponsePart;

fn padded_first_frame(stream: &str, payload_len: usize) -> String {
    let first_end = stream.find("\r\n\r\n").unwrap();
    let payload = &stream[6..first_end];
    assert!(payload.len() <= payload_len);
    format!(
        "data: {payload}{}\r\n\r\n{}",
        " ".repeat(payload_len - payload.len()),
        &stream[first_end + 4..]
    )
}

fn stream_with_json_events(event_count: usize) -> String {
    const BASE_JSON_EVENTS: usize = 9;
    assert!(event_count >= BASE_JSON_EVENTS);
    let (mut stream, mut output) = start_stream();
    for _ in 0..event_count - BASE_JSON_EVENTS {
        push_event(
            &mut stream,
            serde_json::json!({"type":"response.in_progress","response":{"status":"in_progress"}}),
        );
    }
    push_message(&mut stream, &mut output, "message-0", "ok");
    finish_stream(stream, output)
}

fn text_stream(total_bytes: usize) -> String {
    let (mut stream, mut output) = start_stream();
    let first = generated_ascii(b't', total_bytes / 2);
    let second = generated_ascii(b'x', total_bytes - first.len());
    push_message(&mut stream, &mut output, "message-0", &first);
    push_message(&mut stream, &mut output, "message-1", &second);
    finish_stream(stream, output)
}

fn summary_stream(total_bytes: usize) -> String {
    let (mut stream, mut output) = start_stream();
    let first = generated_ascii(b'r', total_bytes / 2);
    let second = generated_ascii(b's', total_bytes - first.len());
    push_reasoning_named(&mut stream, &mut output, "reason-0", &first);
    push_reasoning_named(&mut stream, &mut output, "reason-1", &second);
    finish_stream(stream, output)
}

fn argument_stream(lengths: &[usize]) -> String {
    let arguments: Vec<String> = lengths
        .iter()
        .enumerate()
        .map(|(index, len)| generated_ascii(b'a' + index as u8, *len))
        .collect();
    let references: Vec<&str> = arguments.iter().map(String::as_str).collect();
    strict_stream("", None, &references)
}

fn production_limits_with_frame_headroom() -> ParserLimits {
    // Completed events repeat all output, so aggregate-limit fixtures need frame headroom
    // to reach their independent production boundary.
    ParserLimits {
        frame: 2 * MAX_CODEX_SSE_FRAME_BYTES,
        ..ParserLimits::default()
    }
}

#[test]
fn production_frame_limit_accepts_exact_and_rejects_plus_one() {
    let base = strict_stream("ok", None, &[]);
    let exact = padded_first_frame(&base, MAX_CODEX_SSE_FRAME_BYTES);
    parse_adversarial(&exact, ParserLimits::default()).unwrap();

    let plus_one = padded_first_frame(&base, MAX_CODEX_SSE_FRAME_BYTES + 1);
    assert_eq!(
        invalid_message(parse_adversarial(&plus_one, ParserLimits::default()).unwrap_err()),
        FRAME_LIMIT_ERROR
    );
}

#[test]
fn production_event_limit_accepts_exact_and_rejects_plus_one() {
    let exact = stream_with_json_events(MAX_CODEX_SSE_EVENTS);
    assert_eq!(json_event_count(&exact), MAX_CODEX_SSE_EVENTS);
    assert!(exact.len() <= crate::response::MAX_SUCCESS_BODY_BYTES);
    parse_adversarial(&exact, ParserLimits::default()).unwrap();

    let plus_one = stream_with_json_events(MAX_CODEX_SSE_EVENTS + 1);
    assert_eq!(json_event_count(&plus_one), MAX_CODEX_SSE_EVENTS + 1);
    assert!(plus_one.len() <= crate::response::MAX_SUCCESS_BODY_BYTES);
    assert_eq!(
        invalid_message(parse_adversarial(&plus_one, ParserLimits::default()).unwrap_err()),
        EVENT_LIMIT_ERROR
    );
}

#[test]
fn production_text_limit_accepts_exact_and_rejects_plus_one() {
    let exact = text_stream(MAX_CODEX_TEXT_BYTES);
    let response = parse_adversarial(&exact, production_limits_with_frame_headroom()).unwrap();
    let accepted_bytes: usize = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ModelResponsePart::Text(text) => Some(text.content.len()),
            _ => None,
        })
        .sum();
    assert_eq!(accepted_bytes, MAX_CODEX_TEXT_BYTES);

    let plus_one = text_stream(MAX_CODEX_TEXT_BYTES + 1);
    assert_eq!(
        invalid_message(
            parse_adversarial(&plus_one, production_limits_with_frame_headroom()).unwrap_err()
        ),
        TEXT_LIMIT_ERROR
    );
}

#[test]
fn production_summary_limit_accepts_exact_and_rejects_plus_one() {
    let exact = summary_stream(MAX_CODEX_REASONING_SUMMARY_BYTES);
    let response = parse_adversarial(&exact, production_limits_with_frame_headroom()).unwrap();
    assert!(response.parts.is_empty());

    let plus_one = summary_stream(MAX_CODEX_REASONING_SUMMARY_BYTES + 1);
    assert_eq!(
        invalid_message(
            parse_adversarial(&plus_one, production_limits_with_frame_headroom()).unwrap_err()
        ),
        SUMMARY_LIMIT_ERROR
    );
}

#[test]
fn production_per_call_argument_limit_accepts_exact_and_rejects_plus_one() {
    let exact_args = generated_ascii(b'a', MAX_CODEX_ARGUMENT_BYTES_PER_CALL);
    let exact = strict_stream("", None, &[&exact_args]);
    let response = parse_adversarial(&exact, ParserLimits::default()).unwrap();
    assert!(matches!(
        &response.parts[0],
        ModelResponsePart::ToolCall(call)
            if call.args == ToolCallArgs::String(exact_args)
    ));

    let plus_one_args = generated_ascii(b'a', MAX_CODEX_ARGUMENT_BYTES_PER_CALL + 1);
    let plus_one = strict_stream("", None, &[&plus_one_args]);
    assert_eq!(
        invalid_message(parse_adversarial(&plus_one, ParserLimits::default()).unwrap_err()),
        CALL_ARGUMENT_LIMIT_ERROR
    );
}

#[test]
fn production_aggregate_argument_limit_accepts_exact_and_rejects_plus_one() {
    let half = MAX_CODEX_ARGUMENT_BYTES_TOTAL / 2;
    let exact = argument_stream(&[half, MAX_CODEX_ARGUMENT_BYTES_TOTAL - half]);
    let response = parse_adversarial(&exact, production_limits_with_frame_headroom()).unwrap();
    let accepted_bytes: usize = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ModelResponsePart::ToolCall(call) => Some(match &call.args {
                ToolCallArgs::String(arguments) => arguments.len(),
                ToolCallArgs::Json(_) => unreachable!(),
            }),
            _ => None,
        })
        .sum();
    assert_eq!(accepted_bytes, MAX_CODEX_ARGUMENT_BYTES_TOTAL);

    let plus_one = argument_stream(&[half, half, 1]);
    assert_eq!(
        invalid_message(
            parse_adversarial(&plus_one, production_limits_with_frame_headroom()).unwrap_err()
        ),
        TOTAL_ARGUMENT_LIMIT_ERROR
    );
}

#[test]
fn production_call_limit_accepts_exact_and_rejects_plus_one() {
    let exact_args = vec!["a"; MAX_CODEX_FUNCTION_CALLS];
    let exact = strict_stream("", None, &exact_args);
    let response = parse_adversarial(&exact, ParserLimits::default()).unwrap();
    assert_eq!(
        response
            .parts
            .iter()
            .filter(|part| matches!(part, ModelResponsePart::ToolCall(_)))
            .count(),
        MAX_CODEX_FUNCTION_CALLS
    );

    let plus_one_args = vec!["a"; MAX_CODEX_FUNCTION_CALLS + 1];
    let plus_one = strict_stream("", None, &plus_one_args);
    assert_eq!(
        invalid_message(parse_adversarial(&plus_one, ParserLimits::default()).unwrap_err()),
        CALL_LIMIT_ERROR
    );
}
