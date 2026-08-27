#[cfg(test)]
mod tests {
    use std::env;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::{Component, Path, PathBuf};
    use std::time::Duration;

    use serdes_ai::core::messages::{FinishReason, ModelResponsePart, ToolCallArgs};
    use serdes_ai::core::{
        ModelRequest, ModelRequestPart, ModelResponse, ModelSettings, ToolReturnPart,
    };
    use serdes_ai::models::chatgpt_oauth::{
        ChatGptOAuthModel, CodexReasoning, CodexReasoningEffort, CodexReasoningSummary,
    };
    use serdes_ai::models::{Model as _, ModelError, ModelRequestParameters, ToolChoice};
    use serdes_ai::tools::ToolDefinition;

    use super::super::credentials::{
        parse_credential, Clock, CredentialError, CredentialSource, SystemClock,
        CREDENTIAL_EXPIRY_SKEW_SECONDS,
    };
    use super::super::identity::ProviderSessionId;
    use super::super::macos_keychain::{
        fixed_item_attributes, item_attributes_for_test, read_generic_password_for_test,
        MacOsCredentialSource,
    };
    use super::super::settings::{CodexCacheMode, CodexResponsesSettingsDraft};
    use crate::session::SessionId;

    const GATE: &str = "I_UNDERSTAND";
    const INTEROP_ACCOUNT: &str = "interop-test";
    const INTEROP_FIXTURE: &[u8] = b"llxprt-code-rs-issue1-interop-fixture-v1";
    const TOOL_NAME: &str = "issue1_protocol_probe";
    const TOOL_OUTPUT: &str = "operator-protocol-result-v1";

    struct StructuralClock;

    impl Clock for StructuralClock {
        fn unix_seconds(&self) -> Result<i64, CredentialError> {
            Ok(i64::MIN)
        }
    }

    fn path_is_lexically_safe(path: &Path) -> bool {
        path.is_absolute()
            && path
                .components()
                .all(|component| !matches!(component, Component::ParentDir))
    }

    fn authorized_result_file() -> PathBuf {
        assert!(
            env::var("LLXPRT_ISSUE1_OPERATOR_PROTOCOL").ok().as_deref() == Some(GATE),
            "OPERATOR_PROTOCOL_GATE_REQUIRED"
        );
        let root =
            PathBuf::from(env::var_os("LLXPRT_EVIDENCE_ROOT").expect("EVIDENCE_ROOT_REQUIRED"));
        assert!(path_is_lexically_safe(&root), "EVIDENCE_ROOT_INVALID");
        let root = root.canonicalize().expect("EVIDENCE_ROOT_INVALID");
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("CHECKOUT_ROOT_INVALID");
        let root_is_external =
            root != checkout && !root.starts_with(&checkout) && !checkout.starts_with(&root);
        assert!(root_is_external, "EVIDENCE_ROOT_NOT_EXTERNAL");
        let result = PathBuf::from(
            env::var_os("LLXPRT_OPERATOR_RESULT_FILE").expect("RESULT_FILE_REQUIRED"),
        );
        assert!(path_is_lexically_safe(&result), "RESULT_FILE_INVALID");
        let parent = result.parent().expect("RESULT_FILE_INVALID");
        let parent = parent.canonicalize().expect("RESULT_FILE_INVALID");
        assert!(parent.starts_with(&root), "RESULT_FILE_INVALID");
        parent.join(result.file_name().expect("RESULT_FILE_INVALID"))
    }

    fn emit(path: &Path, marker: &'static str) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("RESULT_FILE_CREATE_FAILED");
        file.write_all(marker.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .expect("RESULT_FILE_WRITE_FAILED");
    }

    fn exact_random_service(value: &str) -> bool {
        let Some(suffix) = value.strip_prefix("llxprt-code-rs-issue1-test-") else {
            return false;
        };
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn fixed_item_shape_marker() -> &'static str {
        let bytes = match read_generic_password_for_test("llxprt-code-oauth", "codex:default") {
            Ok(bytes) => bytes,
            Err(_) => return "SHAPE_PRECONDITION_FAILED",
        };
        let credential = match parse_credential(&bytes, &StructuralClock) {
            Ok(credential) => credential,
            Err(_) => return "SHAPE_INCOMPATIBLE",
        };
        let minimum_expiry = match SystemClock.unix_seconds().and_then(|now| {
            now.checked_add(CREDENTIAL_EXPIRY_SKEW_SECONDS)
                .ok_or_else(CredentialError::remediation)
        }) {
            Ok(minimum_expiry) => minimum_expiry,
            Err(_) => return "SHAPE_PRECONDITION_FAILED",
        };
        if credential.expiry() > minimum_expiry {
            "SHAPE_OK"
        } else {
            "SHAPE_PRECONDITION_FAILED"
        }
    }

    fn smoke_session() -> Option<ProviderSessionId> {
        let label = env::var("LLXPRT_OPERATOR_SESSION_LABEL").ok()?;
        let suffix = label.strip_prefix("issue1-smoke-")?;
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        let session = SessionId::parse(&label).ok()?;
        ProviderSessionId::from_session_id(&session).ok()
    }

    fn smoke_parameters() -> ModelRequestParameters {
        let tool = ToolDefinition::new(TOOL_NAME, "Return the fixed protocol probe result")
            .with_parameters(serde_json::json!({
                "type": "object",
                "properties": {"probe": {"type": "string", "const": "operator-protocol-v1"}},
                "required": ["probe"],
                "additionalProperties": false
            }))
            .with_strict(true);
        ModelRequestParameters::new()
            .with_tools(vec![tool])
            .with_tool_choice(ToolChoice::Specific(TOOL_NAME.to_string()))
    }

    fn initial_history() -> Vec<ModelRequest> {
        let mut system = ModelRequest::new();
        system.add_system_prompt(
            "Call the available protocol probe tool exactly once. Do not use any other tool."
                .to_string(),
        );
        let mut user = ModelRequest::new();
        user.add_user_prompt(
            "Call issue1_protocol_probe once with probe set to operator-protocol-v1.".to_string(),
        );
        vec![system, user]
    }

    fn exact_first_call(response: &ModelResponse) -> Option<String> {
        if response.finish_reason != Some(FinishReason::ToolCall)
            || response
                .parts
                .iter()
                .any(|part| matches!(part, ModelResponsePart::BuiltinToolCall(_)))
        {
            return None;
        }
        let calls = response
            .parts
            .iter()
            .filter_map(|part| match part {
                ModelResponsePart::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [call] = calls.as_slice() else {
            return None;
        };
        let call_id = call.tool_call_id.as_ref()?.clone();
        if call_id.is_empty() || call.tool_name != TOOL_NAME {
            return None;
        }
        let ToolCallArgs::String(arguments) = &call.args else {
            return None;
        };
        let arguments: serde_json::Value = serde_json::from_str(arguments).ok()?;
        (arguments == serde_json::json!({"probe": "operator-protocol-v1"})).then_some(call_id)
    }

    fn final_response_completed(response: &ModelResponse) -> bool {
        response.finish_reason == Some(FinishReason::Stop)
            && response.parts.iter().all(|part| {
                !matches!(
                    part,
                    ModelResponsePart::ToolCall(_) | ModelResponsePart::BuiltinToolCall(_)
                )
            })
    }

    fn mentions_state_requirement(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        (lower.contains("encrypted") || lower.contains("prior reasoning"))
            && (lower.contains("required") || lower.contains("must include"))
    }

    fn classify_model_error(error: &ModelError) -> &'static str {
        match error {
            ModelError::Http { body, .. } if mentions_state_requirement(body) => {
                "SMOKE_STATE_REQUIRED"
            }
            ModelError::Api { message, .. }
            | ModelError::InvalidResponse(message)
            | ModelError::NotSupported(message)
                if mentions_state_requirement(message) =>
            {
                "SMOKE_STATE_REQUIRED"
            }
            ModelError::Http { status, .. }
                if *status == 401 || *status == 429 || *status >= 500 =>
            {
                "SMOKE_INFRASTRUCTURE_FAILURE"
            }
            ModelError::Timeout(_)
            | ModelError::RateLimited { .. }
            | ModelError::Authentication(_)
            | ModelError::Connection(_)
            | ModelError::Network(_)
            | ModelError::Cancelled => "SMOKE_INFRASTRUCTURE_FAILURE",
            _ => "SMOKE_PROTOCOL_REJECTED",
        }
    }

    async fn run_smoke() -> &'static str {
        let credential = match MacOsCredentialSource.load(&SystemClock) {
            Ok(credential) => credential,
            Err(_) => return "SMOKE_PRECONDITION_FAILED",
        };
        let Some(session) = smoke_session() else {
            return "SMOKE_PROTOCOL_REJECTED";
        };
        let reasoning = CodexReasoning {
            effort: CodexReasoningEffort::High,
            summary: CodexReasoningSummary::Auto,
        };
        let settings = CodexResponsesSettingsDraft::new(
            "gpt-5.6-sol".to_string(),
            Some(reasoning),
            CodexCacheMode::TwentyFourHours,
        )
        .finalize(session);
        if settings.model() != "gpt-5.6-sol"
            || settings.endpoint().as_str() != "https://chatgpt.com/backend-api/codex"
            || settings.store()
            || settings.request_timeout() != Duration::from_secs(300)
        {
            return "SMOKE_PROTOCOL_REJECTED";
        }
        let model = ChatGptOAuthModel::new(settings.model(), credential.access_token())
            .with_account_id(credential.account_id())
            .with_request_settings(settings.into_request_settings());
        execute_rounds(&model).await
    }

    async fn execute_rounds(model: &ChatGptOAuthModel) -> &'static str {
        let mut history = initial_history();
        let first = match model
            .request(&history, &ModelSettings::default(), &smoke_parameters())
            .await
        {
            Ok(response) => response,
            Err(error) => return classify_model_error(&error),
        };
        let Some(call_id) = exact_first_call(&first) else {
            return "SMOKE_MODEL_NONCOMPLIANT";
        };
        history.push(ModelRequest::with_parts(vec![
            ModelRequestPart::ModelResponse(Box::new(first)),
        ]));
        let output = ToolReturnPart::success(TOOL_NAME, TOOL_OUTPUT).with_tool_call_id(call_id);
        history.push(ModelRequest::with_parts(vec![
            ModelRequestPart::ToolReturn(output),
        ]));
        match model
            .request(
                &history,
                &ModelSettings::default(),
                &ModelRequestParameters::default(),
            )
            .await
        {
            Ok(response) if final_response_completed(&response) => "SMOKE_PROTOCOL_ACCEPTED",
            Ok(_) => "SMOKE_MODEL_NONCOMPLIANT",
            Err(error) => classify_model_error(&error),
        }
    }

    #[test]
    #[ignore = "operator-authorized native keychain interoperability protocol"]
    fn disposable_keychain_interop() {
        let result_file = authorized_result_file();
        let service = env::var("LLXPRT_OPERATOR_INTEROP_SERVICE").unwrap_or_default();
        let account = env::var("LLXPRT_OPERATOR_INTEROP_ACCOUNT").unwrap_or_default();
        let result = exact_random_service(&service)
            && account == INTEROP_ACCOUNT
            && item_attributes_for_test(&service, &account).is_ok()
            && read_generic_password_for_test(&service, &account)
                .is_ok_and(|bytes| bytes == INTEROP_FIXTURE);
        emit(
            &result_file,
            if result {
                "INTEROP_OK"
            } else {
                "INTEROP_FAILED"
            },
        );
    }

    #[test]
    #[ignore = "operator-authorized fixed-item attributes-only preflight"]
    fn fixed_item_attributes_preflight() {
        let result_file = authorized_result_file();
        emit(
            &result_file,
            if fixed_item_attributes().is_ok() {
                "PREFLIGHT_OK"
            } else {
                "PREFLIGHT_PRECONDITION_FAILED"
            },
        );
    }

    #[test]
    #[ignore = "operator-authorized fixed-item credential shape check"]
    fn fixed_item_credential_shape() {
        let result_file = authorized_result_file();
        emit(&result_file, fixed_item_shape_marker());
    }

    #[test]
    #[ignore = "operator-authorized Codex stateless two-round protocol smoke"]
    fn codex_stateless_two_round_smoke() {
        let result_file = authorized_result_file();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("RUNTIME_CREATE_FAILED");
        emit(&result_file, runtime.block_on(run_smoke()));
    }
}
