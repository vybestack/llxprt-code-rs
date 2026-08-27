use super::*;
use serdes_ai_core::messages::{SystemPromptPart, UserPromptPart};
use serdes_ai_core::{ModelRequest, ModelRequestPart, ModelResponse};

mod limits_fast;
mod limits_production;
mod request;
mod strict;

pub(super) fn request_settings(cache: bool) -> ChatGptOAuthRequestSettings {
    ChatGptOAuthRequestSettings {
        reasoning: Some(CodexReasoning {
            effort: CodexReasoningEffort::High,
            summary: CodexReasoningSummary::Auto,
        }),
        text_verbosity: CodexTextVerbosity::Medium,
        session_id: ChatGptSessionId::new("session_01").unwrap(),
        prompt_cache_key: cache.then(|| ChatGptPromptCacheKey::new("session_01").unwrap()),
    }
}

pub(super) fn configured_model(cache: bool) -> ChatGptOAuthModel {
    ChatGptOAuthModel::new("GPT-5-Codex", "test-token")
        .with_account_id("account-01")
        .with_request_settings(request_settings(cache))
}

pub(super) fn basic_history() -> Vec<ModelRequest> {
    vec![ModelRequest::with_parts(vec![
        ModelRequestPart::SystemPrompt(SystemPromptPart::new("host instructions")),
        ModelRequestPart::UserPrompt(UserPromptPart::new("user prompt")),
    ])]
}

pub(super) fn push_event(stream: &mut String, event: serde_json::Value) {
    stream.push_str("data: ");
    stream.push_str(&serde_json::to_string(&event).unwrap());
    stream.push_str("\r\n\r\n");
}

pub(super) fn push_reasoning(stream: &mut String, output: &mut Vec<serde_json::Value>, text: &str) {
    push_reasoning_named(stream, output, "reason-0", text);
}

pub(super) fn push_reasoning_named(
    stream: &mut String,
    output: &mut Vec<serde_json::Value>,
    id: &str,
    text: &str,
) {
    let index = output.len();
    let added = serde_json::json!({"id":id,"type":"reasoning","summary":[],"status":"in_progress"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.added","output_index":index,"item":added}),
    );
    let empty = serde_json::json!({"type":"summary_text","text":""});
    push_event(
        stream,
        serde_json::json!({"type":"response.reasoning_summary_part.added","item_id":id,"output_index":index,"summary_index":0,"part":empty}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.reasoning_summary_text.delta","item_id":id,"output_index":index,"summary_index":0,"delta":text}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.reasoning_summary_text.done","item_id":id,"output_index":index,"summary_index":0,"text":text}),
    );
    let part = serde_json::json!({"type":"summary_text","text":text});
    push_event(
        stream,
        serde_json::json!({"type":"response.reasoning_summary_part.done","item_id":id,"output_index":index,"summary_index":0,"part":part}),
    );
    let done = serde_json::json!({"id":id,"type":"reasoning","summary":[part],"status":"completed","encrypted_content":"discarded-secret"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.done","output_index":index,"item":done}),
    );
    output.push(done);
}

pub(super) fn push_message(
    stream: &mut String,
    output: &mut Vec<serde_json::Value>,
    id: &str,
    text: &str,
) {
    let index = output.len();
    let added = serde_json::json!({"id":id,"type":"message","role":"assistant","content":[],"status":"in_progress"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.added","output_index":index,"item":added}),
    );
    let empty = serde_json::json!({"type":"output_text","text":"","annotations":[]});
    push_event(
        stream,
        serde_json::json!({"type":"response.content_part.added","item_id":id,"output_index":index,"content_index":0,"part":empty}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.output_text.delta","item_id":id,"output_index":index,"content_index":0,"delta":text}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.output_text.done","item_id":id,"output_index":index,"content_index":0,"text":text}),
    );
    let part = serde_json::json!({"type":"output_text","text":text,"annotations":[]});
    push_event(
        stream,
        serde_json::json!({"type":"response.content_part.done","item_id":id,"output_index":index,"content_index":0,"part":part}),
    );
    let done = serde_json::json!({"id":id,"type":"message","role":"assistant","content":[part],"status":"completed"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.done","output_index":index,"item":done}),
    );
    output.push(done);
}

pub(super) fn push_call(
    stream: &mut String,
    output: &mut Vec<serde_json::Value>,
    number: usize,
    args: &str,
) {
    let index = output.len();
    let id = format!("item-{number}");
    let call_id = format!("call-{number}");
    let name = format!("tool-{number}");
    let added = serde_json::json!({"id":id,"type":"function_call","call_id":call_id,"name":name,"arguments":"","status":"in_progress"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.added","output_index":index,"item":added}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.function_call_arguments.delta","item_id":id,"output_index":index,"delta":args}),
    );
    push_event(
        stream,
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":id,"output_index":index,"arguments":args}),
    );
    let done = serde_json::json!({"id":id,"type":"function_call","call_id":call_id,"name":name,"arguments":args,"status":"completed"});
    push_event(
        stream,
        serde_json::json!({"type":"response.output_item.done","output_index":index,"item":done}),
    );
    output.push(done);
}

pub(super) fn start_stream() -> (String, Vec<serde_json::Value>) {
    let mut stream = String::new();
    push_event(
        &mut stream,
        serde_json::json!({"type":"response.created","response":{"status":"in_progress"}}),
    );
    push_event(
        &mut stream,
        serde_json::json!({"type":"response.in_progress","response":{"status":"in_progress"}}),
    );
    (stream, Vec::new())
}

pub(super) fn finish_stream(mut stream: String, output: Vec<serde_json::Value>) -> String {
    let response = serde_json::json!({"id":"response-1","model":"GPT-5-Codex","status":"completed","error":null,"incomplete_details":null,"output":output,"usage":{"input_tokens":3,"output_tokens":5,"total_tokens":8}});
    push_event(
        &mut stream,
        serde_json::json!({"type":"response.completed","response":response}),
    );
    stream.push_str("data: [DONE]\r\n\r\n");
    stream
}

pub(super) fn strict_stream(text: &str, summary: Option<&str>, arguments: &[&str]) -> String {
    let (mut stream, mut output) = start_stream();
    if let Some(summary) = summary {
        push_reasoning(&mut stream, &mut output, summary);
    }
    push_message(&mut stream, &mut output, "message-0", text);
    for (number, args) in arguments.iter().enumerate() {
        push_call(&mut stream, &mut output, number, args);
    }
    finish_stream(stream, output)
}

pub(super) fn ordered_stream() -> String {
    let (mut stream, mut output) = start_stream();
    push_reasoning(&mut stream, &mut output, "bounded summary");
    push_message(&mut stream, &mut output, "message-before", "before");
    push_call(&mut stream, &mut output, 0, "{malformed");
    push_message(&mut stream, &mut output, "message-after", "after");
    finish_stream(stream, output)
}

pub(super) fn parse_test(stream: &str, limits: ParserLimits) -> Result<ModelResponse, ModelError> {
    super::sse::parse_chunks_with_limits(&[stream.as_bytes()], limits)
}

pub(super) fn parse_adversarial(
    stream: &str,
    limits: ParserLimits,
) -> Result<ModelResponse, ModelError> {
    const WIDTHS: [usize; 10] = [1, 2, 3, 5, 7, 11, 97, 4093, 16_381, 65_521];
    let bytes = stream.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut partition = 0;
    while start < bytes.len() {
        let end = (start + WIDTHS[partition % WIDTHS.len()]).min(bytes.len());
        chunks.push(&bytes[start..end]);
        start = end;
        partition += 1;
    }
    super::sse::parse_chunks_with_limits(&chunks, limits)
}

pub(super) fn test_limits() -> ParserLimits {
    ParserLimits {
        frame: 64 * 1024,
        events: 1024,
        text: 1024,
        summary: 1024,
        arguments_per_call: 1024,
        arguments_total: 1024,
        calls: 16,
    }
}

pub(super) fn invalid_message(error: ModelError) -> String {
    match error {
        ModelError::InvalidResponse(message) => message,
        other => panic!("unexpected error: {other:?}"),
    }
}

pub(super) fn generated_ascii(byte: u8, len: usize) -> String {
    assert!(byte.is_ascii());
    String::from_utf8(vec![byte; len]).unwrap()
}

pub(super) fn json_event_count(stream: &str) -> usize {
    stream
        .split("\r\n\r\n")
        .filter_map(|frame| frame.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .map(|payload| serde_json::from_str::<serde_json::Value>(payload).unwrap())
        .count()
}
