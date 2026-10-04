//! Adapter cap edges, including cross-part and partial-secret regressions.
use llxprt_code_rs::adapter::LlmResult;
use serdes_ai::core::{ModelResponse, ModelResponsePart};

#[test]
fn reasoning_scrubs_before_first_bound_and_across_parts() {
    let cap = llxprt_code_rs::agent::MAX_TURN_ASSISTANT_BYTES;
    let mut response = ModelResponse::new();
    response.add_part(ModelResponsePart::thinking(format!(
        "{}secret-",
        "x".repeat(cap - 18)
    )));
    response.add_part(ModelResponsePart::thinking(format!(
        "thinking-key{}",
        "z".repeat(100)
    )));
    let result = LlmResult::from_response(&response, &["secret-thinking-key".into()]);
    assert!(result.thinking.len() <= cap);
    assert!(!result.thinking.contains("secret-"));
    assert!(result.thinking.ends_with("[truncated]"));
}

#[test]
fn reasoning_secret_corpus_is_removed_before_emission() {
    let corpus = [
        "opaque-secret-28",
        "Bearer token-sentinel",
        "https://user:password@host/path?query=sentinel",
        "Authorization: header-sentinel",
    ];
    let mut response = ModelResponse::new();
    response.add_part(ModelResponsePart::thinking(corpus.join("\n")));
    let result = LlmResult::from_response(&response, &[corpus[0].into()]);
    for forbidden in [
        "opaque-secret-28",
        "token-sentinel",
        "password",
        "query=sentinel",
        "header-sentinel",
    ] {
        assert!(!result.thinking.contains(forbidden), "exposed {forbidden}");
    }
}
