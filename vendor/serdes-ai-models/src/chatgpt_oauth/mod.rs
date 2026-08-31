//! ChatGPT OAuth model implementation.
//!
//! This model uses OAuth access tokens to authenticate with the ChatGPT Codex API.
//! The production endpoint is fixed by [`ChatGptOAuthModel::new`].

use crate::error::ModelError;
use serde_json::{Map, Value};

pub(crate) const MAX_CODEX_SSE_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CODEX_SSE_EVENTS: usize = 65_536;
pub(crate) const MAX_CODEX_TEXT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CODEX_REASONING_SUMMARY_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CODEX_ARGUMENT_BYTES_PER_CALL: usize = 512 * 1024;
pub(crate) const MAX_CODEX_ARGUMENT_BYTES_TOTAL: usize = 1024 * 1024;
pub(crate) const MAX_CODEX_FUNCTION_CALLS: usize = 16;

const FRAME_LIMIT_ERROR: &str = "Codex SSE frame exceeded its byte limit";
const EVENT_LIMIT_ERROR: &str = "Codex SSE event count exceeded its limit";
const TEXT_LIMIT_ERROR: &str = "Codex assistant text exceeded its byte limit";
const SUMMARY_LIMIT_ERROR: &str = "Codex reasoning summary exceeded its byte limit";
const CALL_ARGUMENT_LIMIT_ERROR: &str =
    "Codex function-call arguments exceeded the per-call byte limit";
const TOTAL_ARGUMENT_LIMIT_ERROR: &str =
    "Codex function-call arguments exceeded the aggregate byte limit";
const CALL_LIMIT_ERROR: &str = "Codex function-call count exceeded its limit";
const MALFORMED_SSE_ERROR: &str = "Codex SSE framing was malformed";
const MALFORMED_EVENT_ERROR: &str = "Codex SSE event was malformed";
const EVENT_ORDER_ERROR: &str = "Codex SSE event order was invalid";
const TERMINAL_ERROR: &str = "Codex response did not complete successfully";

#[derive(Clone, Copy)]
struct ParserLimits {
    frame: usize,
    events: usize,
    text: usize,
    summary: usize,
    arguments_per_call: usize,
    arguments_total: usize,
    calls: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            frame: MAX_CODEX_SSE_FRAME_BYTES,
            events: MAX_CODEX_SSE_EVENTS,
            text: MAX_CODEX_TEXT_BYTES,
            summary: MAX_CODEX_REASONING_SUMMARY_BYTES,
            arguments_per_call: MAX_CODEX_ARGUMENT_BYTES_PER_CALL,
            arguments_total: MAX_CODEX_ARGUMENT_BYTES_TOTAL,
            calls: MAX_CODEX_FUNCTION_CALLS,
        }
    }
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, ModelError> {
    object
        .get(key)
        .ok_or_else(|| invalid(MALFORMED_EVENT_ERROR))
        .and_then(as_object)
}

fn required_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Vec<Value>, ModelError> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(MALFORMED_EVENT_ERROR))
}

fn required_str<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, ModelError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(MALFORMED_EVENT_ERROR))
}

fn required_nonempty<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, ModelError> {
    let value = required_str(object, key)?;
    if value.is_empty() {
        Err(invalid(MALFORMED_EVENT_ERROR))
    } else {
        Ok(value)
    }
}

fn required_index(object: &Map<String, Value>, key: &str) -> Result<usize, ModelError> {
    let value = required_u64(object, key)?;
    usize::try_from(value).map_err(|_| invalid(MALFORMED_EVENT_ERROR))
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ModelError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(MALFORMED_EVENT_ERROR))
}

fn require_null_or_absent(object: &Map<String, Value>, key: &str) -> Result<(), ModelError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(_) => Err(invalid(TERMINAL_ERROR)),
    }
}

fn as_object(value: &Value) -> Result<&Map<String, Value>, ModelError> {
    value
        .as_object()
        .ok_or_else(|| invalid(MALFORMED_EVENT_ERROR))
}

fn checked_add(
    current: usize,
    added: usize,
    limit: usize,
    message: &str,
) -> Result<usize, ModelError> {
    match current.checked_add(added) {
        Some(next) if next <= limit => Ok(next),
        _ => Err(invalid(message)),
    }
}

fn invalid(message: &str) -> ModelError {
    ModelError::invalid_response(message)
}

mod model;
mod sse;
mod types;

pub use model::ChatGptOAuthModel;
pub use types::*;

/// Available ChatGPT Codex models.
pub mod models {
    /// ChatGPT 4o Codex.
    pub const CHATGPT_4O_CODEX: &str = "chatgpt-4o-codex";
    /// ChatGPT o1 Codex.
    pub const CHATGPT_O1_CODEX: &str = "chatgpt-o1-codex";
    /// ChatGPT o3 Codex.
    pub const CHATGPT_O3_CODEX: &str = "chatgpt-o3-codex";
    /// ChatGPT o4-mini Codex.
    pub const CHATGPT_O4_MINI_CODEX: &str = "chatgpt-o4-mini-codex";
}

#[cfg(test)]
mod tests;
