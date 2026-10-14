//! Check the conservative framing charge against the production send converters.
use super::*;
use crate::adapter::{
    assistant_request, system_request, tool_return_request, user_request, ToolCall,
};
use serdes_ai::models::{telemetry::Collector, Model, ModelError};

fn requests() -> Vec<serdes_ai::core::ModelRequest> {
    let mut requests = vec![system_request("instructions"), user_request("prompt")];
    for index in 0..120 {
        let id = format!("wire-{index}");
        requests.push(assistant_request(&LlmResult {
            continuation: None,
            thinking: String::new(),
            text: "assistant 界 narration \"quoted\"\n".repeat(10),
            calls: vec![ToolCall {
                id: id.clone(),
                name: "read_file".into(),
                args_json: serde_json::json!({"path":"evidence.txt","max_output_bytes":100})
                    .to_string(),
            }],
            finish_reason: None,
            usage: Default::default(),
        }));
        requests.push(tool_return_request(
            "read_file",
            &id,
            index % 2 == 0,
            "evidence 界 \"quoted\"\n",
        ));
    }
    requests
}

fn models() -> Vec<Box<dyn Model>> {
    vec![
        Box::new(crate::adapter::openai_chat_model(
            "test-model",
            "fixture-key",
            "http://127.0.0.1:9/v1",
            std::time::Duration::from_secs(5),
        )),
        Box::new(
            serdes_ai::models::anthropic::AnthropicModel::new("test-model", "fixture-key")
                .with_base_url("http://127.0.0.1:9"),
        ),
        Box::new(
            serdes_ai::models::openai::OpenAIResponsesModel::new("test-model", "fixture-key")
                .with_base_url("http://127.0.0.1:9/v1"),
        ),
        Box::new(serdes_ai_responses::client::OpenResponsesModel::new(
            "test-model",
            "http://127.0.0.1:9/responses",
        )),
    ]
}

#[test]
fn long_turn_overhead_is_conservative_against_actual_send_projections() {
    let requests = crate::context_kernel::security::quote_requests(&requests());
    let tools = crate::tools::tool_specs(true);
    let params = crate::model::SerdeAiParams {
        tools: std::sync::Arc::new(tools.iter().map(crate::adapter::schema_for).collect()),
    }
    .to_model_request_parameters();
    let estimate =
        estimate_request_bytes(&requests) + crate::adapter::estimate_tool_schema_bytes(&tools);
    let parts = requests
        .iter()
        .flat_map(|r| &r.parts)
        .map(|p| serde_json::to_vec(p).unwrap().len())
        .sum::<usize>();
    let framing = estimate_request_bytes(&requests) - parts;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for model in models() {
        // Capture the exact production projection, then refuse it before network I/O.
        let collector = Collector::with_wire_gate(|_| {
            Err(ModelError::Configuration("fixture capture complete".into()))
        });
        let error = runtime
            .block_on(collector.scope(model.request(&requests, &Default::default(), &params)))
            .unwrap_err();
        match error {
            ModelError::Configuration(message) => assert_eq!(message, "fixture capture complete"),
            other => panic!("unexpected capture failure: {other:?}"),
        }
        let wire = collector.snapshot().wire.unwrap();
        println!(
            "{}: wire={} estimated={} typed_parts={} framing={}",
            model.system(),
            wire.len(),
            estimate,
            parts,
            framing
        );
        assert!(
            wire.len() <= estimate,
            "the complete admission guard must remain conservative"
        );
        assert!(
            framing > 120_000,
            "fixture reproduces the issue's large explicit overhead category"
        );
    }
}
