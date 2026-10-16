//! Strict Responses SSE transport boundary. No payload text enters diagnostics.
use crate::types::{ResponseStatus, StreamEvent};
use serde::Deserialize;
use serdes_ai_models::ModelError;

const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub(super) enum Frame {
    Control,
    Done,
    Event(Box<StreamEvent>),
}

fn invalid(reason: &str) -> ModelError {
    ModelError::ProtocolUnsupported(format!("responses SSE: {reason}"))
}

#[derive(Default)]
pub(super) struct Decoder {
    line: Vec<u8>,
    data: String,
    name: Option<String>,
    has_data: bool,
    bytes: usize,
    after_cr: bool,
}

impl Decoder {
    pub(super) fn byte(&mut self, byte: u8) -> Result<Option<Frame>, ModelError> {
        if self.after_cr {
            self.after_cr = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        self.bytes += 1;
        if self.bytes > MAX_FRAME_BYTES {
            return Err(invalid("frame exceeds byte limit"));
        }
        if byte != b'\n' && byte != b'\r' {
            self.line.push(byte);
            return Ok(None);
        }
        self.after_cr = byte == b'\r';
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|_| invalid("invalid UTF-8"))?;
        if line.is_empty() {
            self.bytes = 0;
            let name = self.name.take();
            let data = std::mem::take(&mut self.data);
            let has_data = std::mem::replace(&mut self.has_data, false);
            if name.is_none() && !has_data {
                return Ok(None); // comment / standard id / retry transport block
            }
            return decode(name.as_deref(), &data, has_data).map(Some);
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.has_data = true;
                self.data.push_str(value);
            }
            "event" => {
                if self.name.replace(value.to_string()).is_some() {
                    return Err(invalid("duplicate event name"));
                }
            }
            "id" if !value.contains('\0') => {}
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {}
            _ => return Err(invalid("unsupported or malformed SSE field")),
        }
        Ok(None)
    }

    pub(super) fn finish(&self) -> Result<(), ModelError> {
        if !self.line.is_empty() || self.has_data || self.name.is_some() {
            Err(invalid("unterminated frame at EOF"))
        } else {
            Ok(())
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderError {
    code: Option<String>,
    message: String,
    param: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn decode(name: Option<&str>, data: &str, has_data: bool) -> Result<Frame, ModelError> {
    if name == Some("keepalive") && (!has_data || data.is_empty()) {
        return Ok(Frame::Control);
    }
    if data == "[DONE]" {
        if name.is_some() {
            return Err(invalid("named DONE frame"));
        }
        return Ok(Frame::Done);
    }
    let value: serde_json::Value = serde_json::from_str::<super::sse_json::Unique>(data)
        .map_err(|_| invalid("malformed or duplicate-key JSON payload"))?
        .0;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("payload is not an object"))?;
    let kind = object
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("missing or invalid event type"))?
        .to_string();
    // Type names are untrusted input too. Never log arbitrary prose, URLs,
    // control characters or an unbounded string from the provider.
    if !valid_event_type(&kind) {
        return Err(invalid("invalid event type identifier"));
    }
    let result = if name.is_some_and(|name| name != kind) {
        Err(invalid("event name disagrees with payload type"))
    } else {
        decode_event(&kind, value)
    };
    result.map_err(|error| match error {
        ModelError::ProtocolUnsupported(detail) => {
            ModelError::ProtocolUnsupported(format!("{detail}; event_type={kind}"))
        }
        other => other,
    })
}

fn valid_event_type(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 128
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Names in semantic families can change response state, tool arguments or the
/// terminal verdict. Unknown members must never disappear as annotations. The
/// reasoning family is reserved as well: it carries private continuation state.
fn semantic_event_type(kind: &str) -> bool {
    [
        "response.output_",
        "response.function_call_",
        "response.content_part",
        "response.reasoning_",
        "response.created",
        "response.in_progress",
        "response.queued",
        "response.completed",
        "response.failed",
        "response.incomplete",
        "response.cancelled",
        "error",
    ]
    .iter()
    .any(|prefix| kind.starts_with(prefix))
}

fn decode_event(kind: &str, mut value: serde_json::Value) -> Result<Frame, ModelError> {
    let object = value
        .as_object_mut()
        .expect("object checked at envelope boundary");
    if kind == "keepalive" {
        if object
            .keys()
            .any(|key| key != "type" && key != "sequence_number" && key != "payload")
            || object
                .get("sequence_number")
                .is_some_and(|v| v.as_u64().is_none())
        {
            return Err(invalid("ambiguous keepalive payload"));
        }
        return Ok(Frame::Control);
    }
    if kind == "response.metadata" {
        // Informational provider annotation (moderation verdicts) that carries no
        // response state. It is discarded unread, so its contents never reach
        // diagnostics, and it cannot mutate the folded response or terminal state.
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "type" | "sequence_number" | "response_id" | "metadata"
            )
        }) || !object.get("metadata").is_some_and(|v| v.is_object())
            || object
                .get("sequence_number")
                .is_some_and(|v| v.as_u64().is_none())
            || object.get("response_id").is_some_and(|v| !v.is_string())
        {
            return Err(invalid("ambiguous metadata event"));
        }
        return Ok(Frame::Control);
    }
    if kind == "error" {
        object.remove("type");
        if let Some(sequence) = object.remove("sequence_number") {
            if sequence.as_u64().is_none() {
                return Err(invalid("invalid error sequence number"));
            }
        }
        let status = object
            .remove("status_code")
            .map(|value| error_status(&value))
            .transpose()?;
        let body = if let Some(body) = object.remove("error") {
            if !object.is_empty() {
                return Err(invalid("ambiguous error envelope"));
            }
            body
        } else {
            value
        };
        let error: ProviderError =
            serde_json::from_value(body).map_err(|_| invalid("malformed provider error"))?;
        // Provider prose/params can echo prompts, tokens or URLs, so they are
        // never retained. The code/type identifiers are sanitized before use.
        let _ = (error.message, error.param);
        return Err(provider_failure(
            error.code.as_deref(),
            error.kind.as_deref(),
            status,
        ));
    }
    if kind == "response.failed" {
        let status = object
            .remove("status_code")
            .map(|value| error_status(&value))
            .transpose()?;
        let event: StreamEvent =
            serde_json::from_value(value).map_err(|_| invalid("malformed failed response"))?;
        let StreamEvent::ResponseFailed { response, .. } = event else {
            unreachable!("closed enum deserialized with response.failed tag")
        };
        if response.status != ResponseStatus::Failed {
            return Err(invalid("failed event has non-failed response status"));
        }
        let code = response.error.map(|error| error.code);
        return Err(provider_failure(code.as_deref(), None, status));
    }
    if !semantic_event_type(kind) {
        // This rule is intentionally name-based, not a payload heuristic. An
        // informational extension cannot supply lifecycle or terminal authority.
        if object
            .get("sequence_number")
            .is_some_and(|v| v.as_u64().is_none())
        {
            return Err(invalid("invalid informational sequence number"));
        }
        tracing::warn!(
            event_type = kind,
            "ignoring informational Responses SSE event"
        );
        return Ok(Frame::Control);
    }
    let event: StreamEvent = serde_json::from_value(value)
        .map_err(|_| invalid("unknown semantic or malformed known Responses event"))?;
    match &event {
        // These lifecycle objects are discarded by the assembler. Validate
        // their nonterminal status/error first so contradictions cannot vanish.
        StreamEvent::ResponseCreated { response, .. }
            if !matches!(
                response.status,
                ResponseStatus::Queued | ResponseStatus::InProgress
            ) || response.error.is_some() =>
        {
            return Err(invalid("ambiguous created response"))
        }
        StreamEvent::ResponseInProgress { response, .. }
            if response.status != ResponseStatus::InProgress || response.error.is_some() =>
        {
            return Err(invalid("ambiguous in-progress response"))
        }
        StreamEvent::ResponseCompleted { response, .. }
            if response.status != ResponseStatus::Completed || response.error.is_some() =>
        {
            return Err(invalid("ambiguous completed response"))
        }
        StreamEvent::ResponseIncomplete { response, .. }
            if response.status != ResponseStatus::Incomplete || response.error.is_some() =>
        {
            return Err(invalid("ambiguous incomplete response"))
        }
        _ => {}
    }
    Ok(Frame::Event(Box::new(event)))
}

fn error_status(value: &serde_json::Value) -> Result<u16, ModelError> {
    value
        .as_u64()
        .filter(|n| (400..=599).contains(n))
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| invalid("invalid error status code"))
}

const MAX_PROVIDER_CODE_CHARS: usize = 64;

/// Provider codes are identifiers, not prose. Anything outside the identifier
/// alphabet or length bound could carry a secret, so it is replaced wholesale.
fn sanitize_provider_code(code: &str) -> &str {
    let valid = !code.is_empty()
        && code.len() <= MAX_PROVIDER_CODE_CHARS
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    if valid {
        code
    } else {
        "invalid_code"
    }
}

/// Known categories only relabel the failure. Every category stays an
/// `ModelError::Api`, which is never replayed or retried.
fn known_category(code: &str) -> Option<&'static str> {
    match code {
        "invalid_request_error" => Some("invalid_request_error"),
        "context_length_exceeded" => Some("context_length_exceeded"),
        "rate_limit_exceeded"
        | "rate_limit_error"
        | "usage_limit_reached"
        | "insufficient_quota"
        | "too_many_requests" => Some("rate_limit_exceeded"),
        "server_error" | "internal_error" | "server_is_overloaded" => Some("server_error"),
        "model_error" => Some("model_error"),
        "authentication_error" => Some("authentication_error"),
        _ => None,
    }
}

fn provider_failure(code: Option<&str>, kind: Option<&str>, status: Option<u16>) -> ModelError {
    let category = [code, kind]
        .into_iter()
        .flatten()
        .find_map(known_category)
        .or((status == Some(429)).then_some("rate_limit_exceeded"))
        .unwrap_or("unclassified");
    let mut detail = category.to_string();
    if let Some(provider_code) = code.or(kind).map(sanitize_provider_code) {
        if provider_code != category {
            detail.push_str("; provider_code=");
            detail.push_str(provider_code);
        }
    }
    if let Some(status) = status {
        detail.push_str(&format!("; status={status}"));
    }
    ModelError::api_with_code(
        format!("responses SSE: provider error ({detail}); request not replayed"),
        format!("sse_provider_{category}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(input: &[u8]) -> Result<Vec<Frame>, ModelError> {
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        for byte in input {
            if let Some(frame) = decoder.byte(*byte)? {
                frames.push(frame);
            }
        }
        decoder.finish()?;
        Ok(frames)
    }

    #[test]
    fn ignored_event_diagnostic_contains_only_the_validated_type() {
        use std::sync::{Arc, Mutex};
        use tracing::{span, Event, Metadata, Subscriber};
        struct Capture(Arc<Mutex<Vec<String>>>);
        impl tracing::field::Visit for Capture {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("{}={value:?}", field.name()));
            }
        }
        impl Subscriber for Capture {
            fn register_callsite(
                &self,
                _: &'static Metadata<'static>,
            ) -> tracing::subscriber::Interest {
                tracing::subscriber::Interest::always()
            }
            fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
                Some(tracing::metadata::LevelFilter::TRACE)
            }
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
                span::Id::from_u64(1)
            }
            fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
            fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
            fn event(&self, event: &Event<'_>) {
                event.record(&mut Capture(self.0.clone()));
            }
            fn enter(&self, _: &span::Id) {}
            fn exit(&self, _: &span::Id) {}
        }
        let captured = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(Capture(captured.clone()), || {
            tracing::callsite::rebuild_interest_cache();
            let data = r#"{"type":"response.annotation.future","payload":{"secret":"SECRET"}}"#;
            assert!(matches!(decode(None, data, true).unwrap(), Frame::Control));
        });
        assert_eq!(
            *captured.lock().unwrap(),
            vec![
                "message=ignoring informational Responses SSE event",
                "event_type=\"response.annotation.future\"",
            ]
        );
    }

    #[test]
    fn unknown_informational_types_are_control_frames_not_semantics() {
        for kind in [
            "response.annotation.future",
            "response.telemetry",
            "ping",
            "mystery",
        ] {
            let value = serde_json::json!({"type":kind,"sequence_number":3,
                "metadata":{"private":"SECRET"},"response":{"status":"failed"}});
            for name in [None, Some(kind)] {
                assert!(matches!(
                    decode(name, &value.to_string(), true).unwrap(),
                    Frame::Control
                ));
            }
        }
    }

    #[test]
    fn semantic_extensions_and_malformed_known_events_are_typed_and_named() {
        for kind in [
            "response.output_future.delta",
            "response.function_call_future.delta",
            "response.content_part.future",
            "response.reasoning_future.delta",
            "response.created.future",
            "response.in_progress.future",
            "response.queued",
            "response.completed.future",
            "response.failed.future",
            "response.incomplete.future",
            "response.cancelled",
            "error.future",
            "response.output_text.delta",
            "response.created",
            "response.in_progress",
            "response.completed",
            "response.failed",
            "response.incomplete",
            "response.metadata",
            "error",
            "keepalive",
        ] {
            let value = serde_json::json!({"type":kind,"unexpected":"SECRET"});
            let error = decode(None, &value.to_string(), true).err().unwrap();
            assert!(!error.is_retryable());
            let ModelError::ProtocolUnsupported(detail) = error else {
                panic!("expected protocol error for {kind}")
            };
            assert!(detail.contains(&format!("event_type={kind}")), "{detail}");
            assert!(!detail.contains("SECRET"));
        }
    }

    #[test]
    fn unsafe_type_names_and_ambiguous_informational_frames_fail_without_echoing_payload() {
        for kind in [
            "",
            "secret=SECRET",
            "line\nbreak",
            "caf\u{e9}",
            &"a".repeat(129),
        ] {
            let value = serde_json::json!({"type":kind,"unexpected":"SECRET"});
            let error = decode(None, &value.to_string(), true).err().unwrap();
            let ModelError::ProtocolUnsupported(detail) = error else {
                panic!("wrong error")
            };
            assert_eq!(detail, "responses SSE: invalid event type identifier");
        }
        for data in [
            r#"{"type":"response.annotation","sequence_number":"SECRET"}"#,
            r#"{"type":"response.annotation","type":"response.annotation"}"#,
            r#"{"type":"response.annotation","metadata":{"x":1,"x":2}}"#,
            r#"{"type":7,"payload":"SECRET"}"#,
            r#"{"payload":"SECRET"}"#,
        ] {
            let error = decode(None, data, true).err().unwrap();
            assert!(matches!(error, ModelError::ProtocolUnsupported(_)));
            if let ModelError::ProtocolUnsupported(detail) = error {
                assert!(!detail.contains("SECRET"));
            }
        }
        let error = decode(Some("SECRET"), r#"{"type":"response.annotation"}"#, true)
            .err()
            .unwrap();
        let ModelError::ProtocolUnsupported(detail) = error else {
            panic!("wrong error")
        };
        assert!(detail.contains("event_type=response.annotation"));
        assert!(!detail.contains("SECRET"));
    }

    #[test]
    fn supported_error_shapes_are_terminal_and_scrubbed() {
        for payload in [
            r#"{"type":"error","code":"rate_limit_exceeded","message":"Bearer SECRET","param":null,"sequence_number":1}"#,
            r#"{"type":"error","status_code":500,"error":{"code":"server_error","message":"https://user:SECRET@example.test/?token=SECRET"}}"#,
            r#"{"type":"error","code":null,"message":"SECRET"}"#,
            r#"{"type":"error","error":{"type":"server_error","code":null,"message":"SECRET","param":null}}"#,
        ] {
            let error = frames(format!("event: error\ndata: {payload}\n\n").as_bytes())
                .err()
                .unwrap();
            let ModelError::Api {
                message: detail,
                code,
            } = error
            else {
                panic!("wrong error type")
            };
            assert!(detail.contains("provider error"));
            assert!(code.unwrap().starts_with("sse_provider_"));
            assert!(!provider_failure(Some("rate_limit_exceeded"), None, None).is_retryable());
            assert!(!detail.contains("SECRET"));
            assert!(detail.len() < 160);
        }
    }

    fn api_failure(payload: &str) -> (String, String) {
        let error = frames(format!("data: {payload}\n\n").as_bytes())
            .err()
            .unwrap();
        assert!(!error.is_retryable(), "{error:?}");
        let ModelError::Api { message, code } = error else {
            panic!("wrong error type")
        };
        (message, code.unwrap())
    }

    #[test]
    fn server_is_overloaded_has_server_error_category_without_replay() {
        for payload in [
            r#"{"type":"error","code":"server_is_overloaded","message":"SECRET"}"#,
            r#"{"type":"error","error":{"code":"server_is_overloaded","message":"SECRET"}}"#,
            r#"{"type":"error","error":{"type":"server_is_overloaded","code":null,"message":"SECRET"}}"#,
        ] {
            let (message, code) = api_failure(payload);
            assert_eq!(code, "sse_provider_server_error");
            assert_eq!(
                message,
                "responses SSE: provider error (server_error; provider_code=server_is_overloaded); request not replayed"
            );
        }
    }

    #[test]
    fn unknown_provider_code_is_preserved_with_status() {
        let (message, code) = api_failure(
            r#"{"type":"error","status_code":418,"error":{"code":"quota.window:v2-x","message":"SECRET","param":"SECRET"}}"#,
        );
        assert_eq!(
            message,
            "responses SSE: provider error (unclassified; provider_code=quota.window:v2-x; status=418); request not replayed"
        );
        assert_eq!(code, "sse_provider_unclassified");
        assert!(!message.contains("SECRET"));
    }

    #[test]
    fn unknown_type_is_kept_when_code_is_absent() {
        let (message, code) = api_failure(
            r#"{"type":"error","error":{"type":"billing_hold","code":null,"message":"SECRET"}}"#,
        );
        assert_eq!(
            message,
            "responses SSE: provider error (unclassified; provider_code=billing_hold); request not replayed"
        );
        assert_eq!(code, "sse_provider_unclassified");
    }

    #[test]
    fn unsafe_or_oversized_provider_codes_become_invalid_code() {
        let long = "a".repeat(MAX_PROVIDER_CODE_CHARS + 1);
        let exact = "a".repeat(MAX_PROVIDER_CODE_CHARS);
        for unsafe_code in [
            "Bearer SECRET",
            "https://user:SECRET@example.test/",
            "tok=SECRET",
            "line\nbreak",
            "caf\u{e9}",
            "",
            long.as_str(),
        ] {
            let payload = serde_json::json!({
                "type": "error", "code": unsafe_code, "message": "SECRET"
            })
            .to_string();
            let (message, code) = api_failure(&payload);
            assert_eq!(
                message,
                "responses SSE: provider error (unclassified; provider_code=invalid_code); request not replayed"
            );
            assert_eq!(code, "sse_provider_unclassified");
        }
        let payload = serde_json::json!({"type": "error", "code": exact, "message": "x"});
        let (message, _) = api_failure(&payload.to_string());
        assert!(
            message.contains(&format!("provider_code={exact}")),
            "{message}"
        );
    }

    #[test]
    fn status_code_is_carried_for_error_events_and_failed_responses() {
        let (message, code) = api_failure(
            r#"{"type":"error","status_code":503,"error":{"code":"server_error","message":"SECRET"}}"#,
        );
        assert_eq!(
            message,
            "responses SSE: provider error (server_error; status=503); request not replayed"
        );
        assert_eq!(code, "sse_provider_server_error");

        let (message, _) = api_failure(
            r#"{"type":"error","status_code":500,"code":"server_error","message":"SECRET"}"#,
        );
        assert!(message.contains("status=500"), "{message}");

        let request =
            serde_json::from_value(serde_json::json!({"model":"fixture","input":[]})).unwrap();
        let mut response = crate::types::ResponseObject::in_progress("r", 1, "fixture", &request);
        response.status = ResponseStatus::Failed;
        response.error = Some(
            serde_json::from_value(
                serde_json::json!({"code":"mystery_failure","message":"SECRET"}),
            )
            .unwrap(),
        );
        let mut event = serde_json::to_value(StreamEvent::ResponseFailed {
            sequence_number: 1,
            response,
        })
        .unwrap();
        let (message, code) = api_failure(&event.to_string());
        assert_eq!(
            message,
            "responses SSE: provider error (unclassified; provider_code=mystery_failure); request not replayed"
        );
        assert_eq!(code, "sse_provider_unclassified");
        event["status_code"] = serde_json::json!(502);
        let (message, _) = api_failure(&event.to_string());
        assert_eq!(
            message,
            "responses SSE: provider error (unclassified; provider_code=mystery_failure; status=502); request not replayed"
        );
        assert!(!message.contains("SECRET"));
        event["status_code"] = serde_json::json!(200);
        assert!(frames(format!("data: {event}\n\n").as_bytes()).is_err());
    }

    #[test]
    fn rate_and_usage_limit_codes_and_status_429_are_rate_limit_category() {
        for code in [
            "rate_limit_exceeded",
            "rate_limit_error",
            "usage_limit_reached",
            "insufficient_quota",
            "too_many_requests",
        ] {
            let payload = serde_json::json!({
                "type": "error", "status_code": 429, "error": {"code": code, "message": "SECRET"}
            });
            let (message, error_code) = api_failure(&payload.to_string());
            assert_eq!(error_code, "sse_provider_rate_limit_exceeded", "{code}");
            assert!(
                message.contains("provider error (rate_limit_exceeded"),
                "{message}"
            );
            assert!(message.contains("status=429"), "{message}");
            assert!(!message.contains("SECRET"));
        }
        let (message, code) = api_failure(
            r#"{"type":"error","status_code":429,"error":{"code":"brand_new_limit","message":"SECRET"}}"#,
        );
        assert_eq!(code, "sse_provider_rate_limit_exceeded");
        assert_eq!(
            message,
            "responses SSE: provider error (rate_limit_exceeded; provider_code=brand_new_limit; status=429); request not replayed"
        );
        let (message, code) = api_failure(
            r#"{"type":"error","error":{"type":"rate_limit_error","code":"vendor_specific","message":"x"}}"#,
        );
        assert_eq!(code, "sse_provider_rate_limit_exceeded");
        assert!(
            message.contains("provider_code=vendor_specific"),
            "{message}"
        );
    }

    #[test]
    fn out_of_range_error_status_is_rejected() {
        for status in ["200", "399", "600", "\"429\"", "-1"] {
            let payload = format!(
                r#"data: {{"type":"error","status_code":{status},"error":{{"code":"x","message":"y"}}}}

"#
            );
            assert!(frames(payload.as_bytes()).is_err(), "accepted {status}");
        }
    }

    #[test]
    fn explicit_controls_and_multiline_crlf() {
        let input = b": heartbeat\r\n\r\nevent: keepalive\r\n\r\ndata:{\r\ndata: \"type\":\"keepalive\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let result = frames(input).unwrap();
        assert_eq!(result.len(), 3);
        assert!(matches!(result[0], Frame::Control));
        assert!(matches!(result[1], Frame::Control));
        assert!(matches!(result[2], Frame::Done));
    }

    #[test]
    fn response_metadata_is_discarded_like_a_control_frame() {
        // Synthetic, redacted shape of the Codex moderation annotation.
        let input = concat!(
            "event: response.metadata\n",
            "data: {\"type\":\"response.metadata\",\"sequence_number\":3,",
            "\"response_id\":\"resp_x\",\"metadata\":{\"openai_chatgpt_moderation_metadata\":",
            "{\"prompt\":{\"protection_type\":\"p\",\"omnimod\":{\"outputs\":[{\"is_blocked\":false,",
            "\"results\":[{\"category_flags\":{\"A1\":false,\"REFUSED\":false}}]}]}},",
            "\"generation\":{},\"tool_call\":{},\"tool_response\":{}}}}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{}}\n\n",
        );
        let result = frames(input.as_bytes()).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|frame| matches!(frame, Frame::Control)));
    }

    #[test]
    fn ambiguous_response_metadata_fails_closed() {
        for input in [
            "data: {\"type\":\"response.metadata\"}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":[]}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{},\"error\":{}}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{},\"response\":{}}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{},\"sequence_number\":\"3\"}\n\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{},\"response_id\":7}\n\n",
            "event: response.created\ndata: {\"type\":\"response.metadata\",\"metadata\":{}}\n\n",
        ] {
            assert!(frames(input.as_bytes()).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn owning_parser_checks_terminal_response_consistency() {
        let request =
            serde_json::from_value(serde_json::json!({"model":"fixture","input":[]})).unwrap();
        let mut response = crate::types::ResponseObject::in_progress("r", 1, "fixture", &request);
        response.status = ResponseStatus::Completed;
        let event = StreamEvent::ResponseCompleted {
            sequence_number: 2,
            response: response.clone(),
        };
        let wire = format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        );
        let parsed = frames(wire.as_bytes()).unwrap();
        assert!(
            matches!(&parsed[0], Frame::Event(event) if **event == StreamEvent::ResponseCompleted { sequence_number: 2, response: response.clone() })
        );
        response.status = ResponseStatus::Failed;
        let event = StreamEvent::ResponseCompleted {
            sequence_number: 2,
            response,
        };
        assert!(
            frames(format!("data: {}\n\n", serde_json::to_string(&event).unwrap()).as_bytes())
                .is_err()
        );
    }

    #[test]
    fn nonterminal_lifecycle_status_and_error_are_checked_before_translation() {
        use crate::types::{ErrorBodyRef, ResponseObject};
        let request =
            serde_json::from_value(serde_json::json!({"model":"fixture","input":[]})).unwrap();
        for created in [true, false] {
            for status in [
                ResponseStatus::Queued,
                ResponseStatus::InProgress,
                ResponseStatus::Completed,
                ResponseStatus::Incomplete,
                ResponseStatus::Failed,
            ] {
                for with_error in [false, true] {
                    let mut response = ResponseObject::in_progress("r", 1, "fixture", &request);
                    response.status = status;
                    if with_error {
                        response.error = Some(ErrorBodyRef {
                            code: "server_error".into(),
                            message: "SECRET".into(),
                        });
                    }
                    // Unrelated metadata remains permitted, including opaque values.
                    response.metadata = Some(
                        serde_json::from_value(serde_json::json!({
                            "opaque": {"type": "error"}
                        }))
                        .unwrap(),
                    );
                    let event = if created {
                        StreamEvent::ResponseCreated {
                            sequence_number: 0,
                            response,
                        }
                    } else {
                        StreamEvent::ResponseInProgress {
                            sequence_number: 0,
                            response,
                        }
                    };
                    let wire = format!("data: {}\n\n", serde_json::to_string(&event).unwrap());
                    let valid = !with_error
                        && (status == ResponseStatus::InProgress
                            || (created && status == ResponseStatus::Queued));
                    let result = frames(wire.as_bytes());
                    assert_eq!(result.is_ok(), valid, "{created:?} {status:?} {with_error}");
                    if let Err(error) = result {
                        assert!(!error.to_string().contains("SECRET"));
                        assert!(!error.is_retryable());
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_malformed_and_ambiguous_frames_fail_closed() {
        for input in [
            r#"data: {"type":"error","type":"keepalive"}

"#,
            r#"data: {"type":"error","error":{"message":"x","message":"y"}}

"#,
            "event: mystery\n\n",
            "data: {\n\n",
            "data: []\n\n",
            "data: {\"type\":\"response.output_mystery.delta\"}\n\n",
            "data: {\"type\":\"error\"}\n\n",
            "event: keepalive\ndata: {\"type\":\"error\",\"message\":\"x\"}\n\n",
            "data: {\"type\":\"keepalive\",\"error\":{}}\n\n",
            "data: {\"type\":\"error\",\"message\":\"x\",\"error\":{\"message\":\"y\"}}\n\n",
            "event: keepalive\nevent: keepalive\n\n",
            "data: [DONE]",
            "data: {\"type\":\"error\",\"message\":7}\n\n",
        ] {
            assert!(frames(input.as_bytes()).is_err(), "accepted {input}");
        }
        assert!(frames(b"data: \xff\n\n").is_err());
        assert!(frames(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
    }
}
