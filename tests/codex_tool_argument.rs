//! Current Codex HTTP framing: argument bytes survive transport, folding and adapter.
mod tool_argument_framing;

use llxprt_code_rs::adapter::LlmResult;
use llxprt_code_rs::agent::parse_object_args;
use serdes_ai::core::{FinishReason, ModelRequest, ModelSettings};
use serdes_ai::models::{Model, ModelRequestParameters};
use tool_argument_framing::{payload, serve};

fn request(model: &serdes_ai_responses::client::OpenResponsesModel) -> LlmResult {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime
        .block_on(model.request(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        ))
        .unwrap();
    LlmResult::from_response(&response, &[])
}

#[test]
fn codex_raw_empty_object_is_not_a_placeholder() {
    for fragments in [vec!["{}", " \n"], vec!["{", "}", "\t"]] {
        let expected = fragments.concat();
        let (model, server) = serve(vec![payload(&[fragments], false)], true);
        let result = request(&model);
        server.join().unwrap();
        assert_eq!(result.finish_reason, Some(FinishReason::ToolCall));
        assert_eq!(result.calls.len(), 1);
        assert_eq!(result.calls[0].args_json, expected);
        assert_eq!(
            parse_object_args(&result.calls[0]).unwrap(),
            serde_json::json!({})
        );
    }
}

#[test]
fn codex_malformed_external_arguments_remain_strict_failures() {
    for fragments in [
        vec!["{}", "{\"path\":\".\"}"],
        vec![""],
        vec!["{\"path\":"],
        vec!["[", "]"],
    ] {
        let expected = fragments.concat();
        let (model, server) = serve(vec![payload(&[fragments], false)], true);
        let result = request(&model);
        server.join().unwrap();
        assert_eq!(result.calls.len(), 1);
        assert_eq!(result.calls[0].args_json, expected);
        assert!(parse_object_args(&result.calls[0]).is_err());
    }
}

#[test]
fn codex_interleaved_calls_reasoning_and_split_utf8_preserve_identity() {
    let fragments = [vec!["{\"path\":", "\"é\"}"], vec!["{\"path\":", "\".\"}"]];
    let (model, server) = serve(vec![payload(&fragments, true)], true);
    let result = request(&model);
    server.join().unwrap();
    assert_eq!(result.finish_reason, Some(FinishReason::ToolCall));
    assert_eq!(result.thinking, "framing é");
    assert_eq!(result.calls.len(), 2);
    for (index, call) in result.calls.iter().enumerate() {
        assert_eq!(call.id, format!("call-{index}"));
        assert_eq!(call.name, "list_directory");
        assert_eq!(call.args_json, fragments[index].concat());
        assert!(parse_object_args(call).is_ok());
    }
    assert_eq!(result.usage.cache_read_tokens, Some(5));
}

#[test]
fn codex_arguments_do_not_cross_request_boundaries() {
    let bodies = ["first", "second"]
        .map(|path| payload(&[vec![&format!("{{\"path\":\"{path}\"}}")]], false));
    let (model, server) = serve(bodies.into(), false);
    for path in ["first", "second"] {
        let result = request(&model);
        assert_eq!(result.calls.len(), 1);
        assert_eq!(
            parse_object_args(&result.calls[0]).unwrap(),
            serde_json::json!({"path":path})
        );
    }
    server.join().unwrap();
}

#[test]
fn typed_initial_empty_placeholder_still_starts_a_delta() {
    use serdes_ai::core::messages::{ToolCallArgs, ToolCallPart, ToolCallPartDelta};
    let mut call = ToolCallPart::new("list_directory", ToolCallArgs::default());
    ToolCallPartDelta::new("{\"path\":").apply(&mut call);
    ToolCallPartDelta::new("\".\"}").apply(&mut call);
    assert_eq!(call.args.to_json_string().unwrap(), "{\"path\":\".\"}");
}
