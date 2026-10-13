use super::*;
use serdes_ai::core::{
    ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ToolCallPart, ToolReturnPart,
    UserPromptPart,
};
fn publication() -> RenderPublication {
    RenderPublication::new(
        Capability {
            version: 1,
            serializer_version: 1,
            tokenizer_version: None,
            margin_version: 1,
            placeholders: true,
            provider_instructions_hash: digest(b"instructions"),
            profile_hash: digest(b"profile"),
            max_output_tokens: Some(1024),
        },
        b"[]",
    )
    .unwrap()
}
#[test]
fn render_dependencies_and_context_versions_change_identity() {
    let original = publication();
    let requests = vec![user("hello")];
    let hash = original.request_hash(&requests, 1).unwrap();
    assert_ne!(hash, original.request_hash(&requests, 2).unwrap());
    for field in 0..7 {
        let mut capability = original.capability.clone();
        match field {
            0 => capability.version += 1,
            1 => capability.serializer_version += 1,
            2 => capability.margin_version += 1,
            3 => capability.tokenizer_version = Some("new".into()),
            4 => capability.provider_instructions_hash = digest(b"changed"),
            5 => capability.profile_hash = digest(b"changed"),
            _ => capability.max_output_tokens = Some(2048),
        }
        let changed = RenderPublication::new(capability, b"[]").unwrap();
        assert_ne!(hash, changed.request_hash(&requests, 1).unwrap());
    }
    let changed = RenderPublication::new(original.capability.clone(), b"[tool]").unwrap();
    assert_ne!(hash, changed.request_hash(&requests, 1).unwrap());
    let mut corrupt = original;
    corrupt.tools_hash = digest(b"substituted");
    assert!(corrupt.validate().is_err());
}
#[test]
fn call_result_order_pairing_and_identity_are_send_preconditions() {
    let mut response = ModelResponse::new();
    response.parts.push(ModelResponsePart::ToolCall(
        ToolCallPart::new("read_file", "{}").with_tool_call_id("one"),
    ));
    let call = ModelRequest::with_parts(vec![ModelRequestPart::ModelResponse(Box::new(response))]);
    let result = ModelRequest::with_parts(vec![ModelRequestPart::ToolReturn(
        ToolReturnPart::success("read_file", "ok").with_tool_call_id("one"),
    )]);
    assert!(legal_requests(&[call.clone()]).is_err());
    assert!(legal_requests(&[result.clone()]).is_err());
    assert!(legal_requests(&[call.clone(), user("intruder"), result.clone()]).is_err());
    assert!(legal_requests(&[call.clone(), result.clone()]).is_ok());
    assert!(legal_requests(&[call.clone(), result.clone(), call, result]).is_err());
}
#[test]
fn provider_classes_do_not_infer_unattended_safety() {
    assert!(ProviderClass::StatelessAttested.unattended());
    assert!(ProviderClass::IdempotentIdentity.unattended());
    assert!(!ProviderClass::AtMostOneAttempt.unattended());
    assert!(!ProviderClass::Unattested.unattended());
}

fn user(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::UserPrompt(UserPromptPart::new(
        text,
    ))])
}
