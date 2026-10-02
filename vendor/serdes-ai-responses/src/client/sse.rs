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
    ModelError::InvalidResponse(format!("responses SSE: {reason}"))
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
    let mut value: serde_json::Value = serde_json::from_str::<super::sse_json::Unique>(data)
        .map_err(|_| invalid("malformed or duplicate-key JSON payload"))?
        .0;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("payload is not an object"))?;
    let kind = object
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("missing or invalid event type"))?
        .to_string();
    if name.is_some_and(|name| name != kind) {
        return Err(invalid("event name disagrees with payload type"));
    }
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
    if kind == "error" {
        object.remove("type");
        if let Some(sequence) = object.remove("sequence_number") {
            if sequence.as_u64().is_none() {
                return Err(invalid("invalid error sequence number"));
            }
        }
        let body = if let Some(body) = object.remove("error") {
            if let Some(status) = object.remove("status_code") {
                if !status.as_u64().is_some_and(|n| (400..=599).contains(&n)) {
                    return Err(invalid("invalid error status code"));
                }
            }
            if !object.is_empty() {
                return Err(invalid("ambiguous error envelope"));
            }
            body
        } else {
            value
        };
        let error: ProviderError =
            serde_json::from_value(body).map_err(|_| invalid("malformed provider error"))?;
        // Provider prose/params can echo prompts, tokens or URLs. Retain only
        // known categories; unknown codes are never printed or used for replay.
        let _ = (error.message, error.param);
        return Err(provider_failure(
            error.code.as_deref().or(error.kind.as_deref()),
        ));
    }
    if kind == "response.failed" {
        let event: StreamEvent =
            serde_json::from_value(value).map_err(|_| invalid("malformed failed response"))?;
        let StreamEvent::ResponseFailed { response, .. } = event else {
            unreachable!("closed enum deserialized with response.failed tag")
        };
        if response.status != ResponseStatus::Failed {
            return Err(invalid("failed event has non-failed response status"));
        }
        let code = response.error.map(|error| error.code);
        return Err(provider_failure(code.as_deref()));
    }
    let event: StreamEvent = serde_json::from_value(value)
        .map_err(|_| invalid("unknown or malformed Responses event"))?;
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

fn provider_failure(code: Option<&str>) -> ModelError {
    let category = match code {
        Some("invalid_request_error") => "invalid_request_error",
        Some("context_length_exceeded") => "context_length_exceeded",
        Some("rate_limit_exceeded") => "rate_limit_exceeded",
        Some("server_error" | "internal_error") => "server_error",
        Some("model_error") => "model_error",
        Some("authentication_error") => "authentication_error",
        _ => "unclassified",
    };
    ModelError::api_with_code(
        format!("responses SSE: provider error ({category}); request not replayed"),
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
            assert!(!provider_failure(Some("rate_limit_exceeded")).is_retryable());
            assert!(!detail.contains("SECRET"));
            assert!(detail.len() < 160);
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
            "data: {\"type\":\"mystery\"}\n\n",
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
