//! Incremental strict SSE parsing for the ChatGPT Codex Responses endpoint.

use super::{
    as_object, checked_add, invalid, require_null_or_absent, required_array, required_index,
    required_nonempty, required_object, required_str, required_u64, ParserLimits,
    CALL_ARGUMENT_LIMIT_ERROR, CALL_LIMIT_ERROR, EVENT_LIMIT_ERROR, EVENT_ORDER_ERROR,
    FRAME_LIMIT_ERROR, MALFORMED_EVENT_ERROR, MALFORMED_SSE_ERROR, SUMMARY_LIMIT_ERROR,
    TERMINAL_ERROR, TEXT_LIMIT_ERROR, TOTAL_ARGUMENT_LIMIT_ERROR,
};
use crate::error::ModelError;
use crate::response::{Utf8StreamDecoder, MAX_STREAM_BUFFER_BYTES};
use futures::StreamExt as _;
use serde_json::{Map, Value};
use serdes_ai_core::messages::{TextPart, ToolCallArgs, ToolCallPart};
use serdes_ai_core::{FinishReason, ModelResponse, ModelResponsePart, RequestUsage};
use std::collections::BTreeMap;

#[derive(Debug)]
enum ItemState {
    Message {
        id: String,
        text: String,
        content_started: bool,
        text_done: bool,
        content_done: bool,
        item_done: bool,
    },
    Call {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        arguments_done: bool,
        item_done: bool,
    },
    Reasoning {
        id: String,
        summary: String,
        part_started: bool,
        text_done: bool,
        part_done: bool,
        item_done: bool,
    },
}

impl ItemState {
    fn id(&self) -> &str {
        match self {
            Self::Message { id, .. } | Self::Call { id, .. } | Self::Reasoning { id, .. } => id,
        }
    }

    fn is_done(&self) -> bool {
        match self {
            Self::Message { item_done, .. }
            | Self::Call { item_done, .. }
            | Self::Reasoning { item_done, .. } => *item_done,
        }
    }
}

struct EventParser {
    limits: ParserLimits,
    line: String,
    frame: Option<String>,
    events: usize,
    created: bool,
    completed: bool,
    done_marker: bool,
    items: BTreeMap<usize, ItemState>,
    text_bytes: usize,
    summary_bytes: usize,
    argument_bytes: usize,
    call_count: usize,
    model_name: Option<String>,
    vendor_id: Option<String>,
    usage: Option<RequestUsage>,
}

impl EventParser {
    fn new(limits: ParserLimits) -> Self {
        Self {
            limits,
            line: String::new(),
            frame: None,
            events: 0,
            created: false,
            completed: false,
            done_marker: false,
            items: BTreeMap::new(),
            text_bytes: 0,
            summary_bytes: 0,
            argument_bytes: 0,
            call_count: 0,
            model_name: None,
            vendor_id: None,
            usage: None,
        }
    }

    fn push_text(&mut self, mut text: &str) -> Result<(), ModelError> {
        while let Some(newline) = text.find('\n') {
            self.push_line_segment(&text[..newline])?;
            self.finish_line()?;
            text = &text[newline + 1..];
        }
        self.push_line_segment(text)
    }

    fn push_line_segment(&mut self, segment: &str) -> Result<(), ModelError> {
        self.line.push_str(segment);
        let line = self.line.strip_suffix('\r').unwrap_or(&self.line);
        let payload = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
            .unwrap_or(line);
        if payload.len() > self.limits.frame {
            return Err(invalid(FRAME_LIMIT_ERROR));
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), ModelError> {
        let mut line = std::mem::take(&mut self.line);
        if line.ends_with('\r') {
            line.pop();
        }
        if line.contains('\r') {
            return Err(invalid(MALFORMED_SSE_ERROR));
        }
        if line.is_empty() {
            return self.finish_frame();
        }
        if line.starts_with(':') {
            if self.frame.is_some() {
                return Err(invalid(MALFORMED_SSE_ERROR));
            }
            return Ok(());
        }
        let data = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
            .ok_or_else(|| invalid(MALFORMED_SSE_ERROR))?;
        if self.frame.replace(data.to_string()).is_some() {
            return Err(invalid(MALFORMED_SSE_ERROR));
        }
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<(), ModelError> {
        let Some(data) = self.frame.take() else {
            return Ok(());
        };
        if data == "[DONE]" {
            if !self.completed || self.done_marker {
                return Err(invalid(EVENT_ORDER_ERROR));
            }
            self.done_marker = true;
            return Ok(());
        }
        if self.completed || self.done_marker {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        self.events = checked_add(self.events, 1, self.limits.events, EVENT_LIMIT_ERROR)?;
        let event: Value =
            serde_json::from_str(&data).map_err(|_| invalid(MALFORMED_EVENT_ERROR))?;
        self.handle_event(&event)
    }

    fn handle_event(&mut self, event: &Value) -> Result<(), ModelError> {
        let object = as_object(event)?;
        match required_str(object, "type")? {
            "response.created" => self.handle_created(object),
            "response.in_progress" => self.handle_in_progress(object),
            "response.output_item.added" => self.handle_item_added(object),
            "response.output_item.done" => self.handle_item_done(object),
            "response.content_part.added" => self.handle_content_added(object),
            "response.content_part.done" => self.handle_content_done(object),
            "response.output_text.delta" => self.handle_text_delta(object),
            "response.output_text.done" => self.handle_text_done(object),
            "response.reasoning_summary_part.added" => self.handle_summary_part_added(object),
            "response.reasoning_summary_part.done" => self.handle_summary_part_done(object),
            "response.reasoning_summary_text.delta" => self.handle_summary_delta(object),
            "response.reasoning_summary_text.done" => self.handle_summary_done(object),
            "response.reasoning_text.delta" | "response.reasoning_text.done" => {
                self.handle_discarded_reasoning(object)
            }
            "response.function_call_arguments.delta" => self.handle_arguments_delta(object),
            "response.function_call_arguments.done" => self.handle_arguments_done(object),
            "response.completed" => self.handle_completed(object),
            "response.failed" | "response.cancelled" | "response.incomplete" => {
                Err(invalid(TERMINAL_ERROR))
            }
            _ => Err(invalid(MALFORMED_EVENT_ERROR)),
        }
    }

    fn handle_created(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        if self.created || !self.items.is_empty() {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        require_response_status(event, "in_progress")?;
        self.created = true;
        Ok(())
    }

    fn handle_in_progress(&self, event: &Map<String, Value>) -> Result<(), ModelError> {
        self.require_active()?;
        require_response_status(event, "in_progress")
    }

    fn handle_item_added(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        self.require_active()?;
        let index = required_index(event, "output_index")?;
        if index != self.items.len() || self.items.contains_key(&index) {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        let item = required_object(event, "item")?;
        let id = required_nonempty(item, "id")?.to_string();
        let state = match required_str(item, "type")? {
            "message" => {
                require_status(item, "in_progress")?;
                if required_str(item, "role")? != "assistant"
                    || !required_array(item, "content")?.is_empty()
                {
                    return Err(invalid(MALFORMED_EVENT_ERROR));
                }
                ItemState::Message {
                    id,
                    text: String::new(),
                    content_started: false,
                    text_done: false,
                    content_done: false,
                    item_done: false,
                }
            }
            "function_call" => self.new_call(item, id)?,
            "reasoning" => {
                require_status(item, "in_progress")?;
                if !required_array(item, "summary")?.is_empty() {
                    return Err(invalid(MALFORMED_EVENT_ERROR));
                }
                ItemState::Reasoning {
                    id,
                    summary: String::new(),
                    part_started: false,
                    text_done: false,
                    part_done: false,
                    item_done: false,
                }
            }
            _ => return Err(invalid(MALFORMED_EVENT_ERROR)),
        };
        if matches!(state, ItemState::Call { .. }) {
            self.call_count += 1;
        }
        self.items.insert(index, state);
        Ok(())
    }

    fn new_call(&self, item: &Map<String, Value>, id: String) -> Result<ItemState, ModelError> {
        require_status(item, "in_progress")?;
        if !required_str(item, "arguments")?.is_empty() {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        }
        if self.call_count >= self.limits.calls {
            return Err(invalid(CALL_LIMIT_ERROR));
        }
        let call_id = required_nonempty(item, "call_id")?.to_string();
        if self.items.values().any(|state| {
            matches!(state, ItemState::Call { call_id: existing, .. } if existing == &call_id)
        }) {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        }
        Ok(ItemState::Call {
            id,
            call_id,
            name: required_nonempty(item, "name")?.to_string(),
            arguments: String::new(),
            arguments_done: false,
            item_done: false,
        })
    }

    fn handle_content_added(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let state = self.item_mut(event)?;
        let ItemState::Message {
            content_started, ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if *content_started || required_index(event, "content_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        require_text_part(required_object(event, "part")?, "")?;
        *content_started = true;
        Ok(())
    }

    fn handle_text_delta(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let delta = required_str(event, "delta")?.to_string();
        let next_text_bytes = checked_add(
            self.text_bytes,
            delta.len(),
            self.limits.text,
            TEXT_LIMIT_ERROR,
        )?;
        let state = self.item_mut(event)?;
        let ItemState::Message {
            text,
            content_started,
            text_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*content_started || *text_done || required_index(event, "content_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        text.push_str(&delta);
        self.text_bytes = next_text_bytes;
        Ok(())
    }

    fn handle_text_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let complete = required_str(event, "text")?.to_string();
        let state = self.item_mut(event)?;
        let ItemState::Message {
            text,
            content_started,
            text_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*content_started
            || *text_done
            || required_index(event, "content_index")? != 0
            || text != &complete
        {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        *text_done = true;
        Ok(())
    }

    fn handle_content_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let part = required_object(event, "part")?;
        let state = self.item_mut(event)?;
        let ItemState::Message {
            text,
            text_done,
            content_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*text_done || *content_done || required_index(event, "content_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        require_text_part(part, text)?;
        *content_done = true;
        Ok(())
    }

    fn handle_summary_part_added(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let state = self.item_mut(event)?;
        let ItemState::Reasoning { part_started, .. } = state else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if *part_started || required_index(event, "summary_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        require_summary_part(required_object(event, "part")?, "")?;
        *part_started = true;
        Ok(())
    }

    fn handle_summary_delta(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let delta = required_str(event, "delta")?.to_string();
        let next_summary_bytes = checked_add(
            self.summary_bytes,
            delta.len(),
            self.limits.summary,
            SUMMARY_LIMIT_ERROR,
        )?;
        let state = self.item_mut(event)?;
        let ItemState::Reasoning {
            summary,
            part_started,
            text_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*part_started || *text_done || required_index(event, "summary_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        summary.push_str(&delta);
        self.summary_bytes = next_summary_bytes;
        Ok(())
    }

    fn handle_summary_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let complete = required_str(event, "text")?.to_string();
        let state = self.item_mut(event)?;
        let ItemState::Reasoning {
            summary,
            part_started,
            text_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*part_started
            || *text_done
            || required_index(event, "summary_index")? != 0
            || summary != &complete
        {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        *text_done = true;
        Ok(())
    }

    fn handle_summary_part_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let part = required_object(event, "part")?;
        let state = self.item_mut(event)?;
        let ItemState::Reasoning {
            summary,
            text_done,
            part_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if !*text_done || *part_done || required_index(event, "summary_index")? != 0 {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        require_summary_part(part, summary)?;
        *part_done = true;
        Ok(())
    }

    fn handle_discarded_reasoning(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let state = self.item_mut(event)?;
        if !matches!(state, ItemState::Reasoning { .. }) {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        }
        if let Some(delta) = event.get("delta") {
            if !delta.is_string() {
                return Err(invalid(MALFORMED_EVENT_ERROR));
            }
        } else if let Some(text) = event.get("text") {
            if !text.is_string() {
                return Err(invalid(MALFORMED_EVENT_ERROR));
            }
        } else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        }
        Ok(())
    }

    fn handle_arguments_delta(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let delta = required_str(event, "delta")?.to_string();
        let next_argument_bytes = checked_add(
            self.argument_bytes,
            delta.len(),
            self.limits.arguments_total,
            TOTAL_ARGUMENT_LIMIT_ERROR,
        )?;
        let per_call = self.limits.arguments_per_call;
        let state = self.item_mut(event)?;
        let ItemState::Call {
            arguments,
            arguments_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if *arguments_done {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        checked_add(
            arguments.len(),
            delta.len(),
            per_call,
            CALL_ARGUMENT_LIMIT_ERROR,
        )?;
        arguments.push_str(&delta);
        self.argument_bytes = next_argument_bytes;
        Ok(())
    }

    fn handle_arguments_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        let complete = required_str(event, "arguments")?.to_string();
        let state = self.item_mut(event)?;
        let ItemState::Call {
            arguments,
            arguments_done,
            ..
        } = state
        else {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        };
        if *arguments_done || arguments != &complete {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        *arguments_done = true;
        Ok(())
    }

    fn handle_item_done(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        self.require_active()?;
        let index = required_index(event, "output_index")?;
        let item = required_object(event, "item")?;
        let item_id = required_nonempty(item, "id")?;
        let state = self
            .items
            .get_mut(&index)
            .ok_or_else(|| invalid(EVENT_ORDER_ERROR))?;
        if state.id() != item_id || state.is_done() {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        match state {
            ItemState::Message {
                id,
                text,
                content_done,
                item_done,
                ..
            } => {
                if !*content_done || *item_done {
                    return Err(invalid(EVENT_ORDER_ERROR));
                }
                validate_message(item, id, text)?;
                *item_done = true;
            }
            ItemState::Call {
                id,
                call_id,
                name,
                arguments,
                arguments_done,
                item_done,
            } => {
                if !*arguments_done || *item_done {
                    return Err(invalid(EVENT_ORDER_ERROR));
                }
                validate_call(item, id, call_id, name, arguments)?;
                *item_done = true;
            }
            ItemState::Reasoning {
                id,
                summary,
                part_started,
                part_done,
                item_done,
                ..
            } => {
                if (*part_started && !*part_done) || *item_done {
                    return Err(invalid(EVENT_ORDER_ERROR));
                }
                validate_reasoning(item, id, summary)?;
                *item_done = true;
            }
        }
        Ok(())
    }

    fn handle_completed(&mut self, event: &Map<String, Value>) -> Result<(), ModelError> {
        self.require_active()?;
        if self.completed || self.items.values().any(|item| !item.is_done()) {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        let response = required_object(event, "response")?;
        require_status(response, "completed")?;
        require_null_or_absent(response, "error")?;
        require_null_or_absent(response, "incomplete_details")?;
        self.validate_final_output(required_array(response, "output")?)?;
        self.vendor_id = Some(required_nonempty(response, "id")?.to_string());
        self.model_name = Some(required_nonempty(response, "model")?.to_string());
        self.usage = parse_usage(response.get("usage"))?;
        self.completed = true;
        Ok(())
    }

    fn validate_final_output(&self, output: &[Value]) -> Result<(), ModelError> {
        if output.len() != self.items.len() {
            return Err(invalid(MALFORMED_EVENT_ERROR));
        }
        for (index, value) in output.iter().enumerate() {
            let item = as_object(value)?;
            match self
                .items
                .get(&index)
                .ok_or_else(|| invalid(EVENT_ORDER_ERROR))?
            {
                ItemState::Message { id, text, .. } => validate_message(item, id, text)?,
                ItemState::Call {
                    id,
                    call_id,
                    name,
                    arguments,
                    ..
                } => validate_call(item, id, call_id, name, arguments)?,
                ItemState::Reasoning { id, summary, .. } => validate_reasoning(item, id, summary)?,
            }
        }
        Ok(())
    }

    fn item_mut(&mut self, event: &Map<String, Value>) -> Result<&mut ItemState, ModelError> {
        self.require_active()?;
        let index = required_index(event, "output_index")?;
        let item_id = required_nonempty(event, "item_id")?;
        let state = self
            .items
            .get_mut(&index)
            .ok_or_else(|| invalid(EVENT_ORDER_ERROR))?;
        if state.id() != item_id || state.is_done() {
            return Err(invalid(EVENT_ORDER_ERROR));
        }
        Ok(state)
    }

    fn require_active(&self) -> Result<(), ModelError> {
        if !self.created || self.completed {
            Err(invalid(EVENT_ORDER_ERROR))
        } else {
            Ok(())
        }
    }

    fn finish(self) -> Result<ModelResponse, ModelError> {
        if !self.line.is_empty() || self.frame.is_some() {
            return Err(invalid(MALFORMED_SSE_ERROR));
        }
        if !self.completed || !self.done_marker {
            return Err(invalid(TERMINAL_ERROR));
        }
        let mut parts = Vec::new();
        for item in self.items.into_values() {
            match item {
                ItemState::Message { text, .. } if !text.is_empty() => {
                    parts.push(ModelResponsePart::Text(TextPart::new(text)));
                }
                ItemState::Call {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    parts.push(ModelResponsePart::ToolCall(
                        ToolCallPart::new(name, ToolCallArgs::String(arguments))
                            .with_tool_call_id(call_id),
                    ));
                }
                ItemState::Message { .. } | ItemState::Reasoning { .. } => {}
            }
        }
        let finish_reason = if parts
            .iter()
            .any(|part| matches!(part, ModelResponsePart::ToolCall(_)))
        {
            FinishReason::ToolCall
        } else {
            FinishReason::Stop
        };
        Ok(ModelResponse {
            parts,
            model_name: self.model_name,
            timestamp: chrono::Utc::now(),
            finish_reason: Some(finish_reason),
            usage: self.usage,
            vendor_id: self.vendor_id,
            vendor_details: None,
            kind: "response".to_string(),
        })
    }
}

struct CodexSseReader {
    decoder: Utf8StreamDecoder,
    parser: EventParser,
}

impl CodexSseReader {
    fn new(limits: ParserLimits) -> Self {
        Self {
            decoder: Utf8StreamDecoder::default(),
            parser: EventParser::new(limits),
        }
    }

    fn push_transport_chunk(&mut self, chunk: &[u8]) -> Result<(), ModelError> {
        for slice in chunk.chunks(MAX_STREAM_BUFFER_BYTES - 3) {
            let mut scratch = String::new();
            self.decoder.push(slice, &mut scratch)?;
            self.parser.push_text(&scratch)?;
            scratch.clear();
        }
        Ok(())
    }

    fn finish(self) -> Result<ModelResponse, ModelError> {
        self.decoder.finish()?;
        self.parser.finish()
    }
}

pub(super) async fn parse_response(
    response: reqwest::Response,
) -> Result<ModelResponse, ModelError> {
    let mut reader = CodexSseReader::new(ParserLimits::default());
    let mut stream = crate::response::stream(response);
    while let Some(chunk) = stream.next().await {
        reader.push_transport_chunk(&chunk?)?;
    }
    reader.finish()
}

#[cfg(test)]
pub(super) fn parse_chunks_with_limits(
    chunks: &[&[u8]],
    limits: ParserLimits,
) -> Result<ModelResponse, ModelError> {
    let mut reader = CodexSseReader::new(limits);
    for chunk in chunks {
        reader.push_transport_chunk(chunk)?;
    }
    reader.finish()
}

fn validate_message(item: &Map<String, Value>, id: &str, text: &str) -> Result<(), ModelError> {
    if required_str(item, "type")? != "message"
        || required_nonempty(item, "id")? != id
        || required_str(item, "role")? != "assistant"
    {
        return Err(invalid(MALFORMED_EVENT_ERROR));
    }
    require_status(item, "completed")?;
    let content = required_array(item, "content")?;
    if content.len() != 1 {
        return Err(invalid(MALFORMED_EVENT_ERROR));
    }
    require_text_part(as_object(&content[0])?, text)
}

fn validate_call(
    item: &Map<String, Value>,
    id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> Result<(), ModelError> {
    if required_str(item, "type")? != "function_call"
        || required_nonempty(item, "id")? != id
        || required_nonempty(item, "call_id")? != call_id
        || required_nonempty(item, "name")? != name
        || required_str(item, "arguments")? != arguments
    {
        return Err(invalid(MALFORMED_EVENT_ERROR));
    }
    require_status(item, "completed")
}

fn validate_reasoning(
    item: &Map<String, Value>,
    id: &str,
    summary: &str,
) -> Result<(), ModelError> {
    if required_str(item, "type")? != "reasoning" || required_nonempty(item, "id")? != id {
        return Err(invalid(MALFORMED_EVENT_ERROR));
    }
    require_status(item, "completed")?;
    let values = required_array(item, "summary")?;
    match (values.as_slice(), summary.is_empty()) {
        ([], true) => Ok(()),
        ([value], false) => require_summary_part(as_object(value)?, summary),
        _ => Err(invalid(MALFORMED_EVENT_ERROR)),
    }
}

fn require_text_part(part: &Map<String, Value>, text: &str) -> Result<(), ModelError> {
    if required_str(part, "type")? == "output_text" && required_str(part, "text")? == text {
        Ok(())
    } else {
        Err(invalid(MALFORMED_EVENT_ERROR))
    }
}

fn require_summary_part(part: &Map<String, Value>, text: &str) -> Result<(), ModelError> {
    if required_str(part, "type")? == "summary_text" && required_str(part, "text")? == text {
        Ok(())
    } else {
        Err(invalid(MALFORMED_EVENT_ERROR))
    }
}

fn require_response_status(event: &Map<String, Value>, expected: &str) -> Result<(), ModelError> {
    require_status(required_object(event, "response")?, expected)
}

fn require_status(object: &Map<String, Value>, expected: &str) -> Result<(), ModelError> {
    if required_str(object, "status")? == expected {
        Ok(())
    } else {
        Err(invalid(TERMINAL_ERROR))
    }
}

fn parse_usage(value: Option<&Value>) -> Result<Option<RequestUsage>, ModelError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let usage = as_object(value)?;
    Ok(Some(RequestUsage {
        request_tokens: Some(required_u64(usage, "input_tokens")?),
        response_tokens: Some(required_u64(usage, "output_tokens")?),
        total_tokens: Some(required_u64(usage, "total_tokens")?),
        cache_creation_tokens: None,
        cache_read_tokens: None,
        details: None,
    }))
}
