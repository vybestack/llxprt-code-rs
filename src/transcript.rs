//! Opt-in live transcript. Reasoning is live-only, never part of session storage.
mod lookback;
#[cfg(test)]
mod tests;
use clap::ValueEnum;
pub use lookback::{Command, Options};
use serde_json::json;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Emit {
    Thinking,
    Text,
    Calls,
    Results,
    All,
}

#[derive(Default)]
pub(crate) struct Emitter {
    categories: Vec<Emit>,
    assistant_bytes: usize,
    args_bytes: usize,
    output_bytes: usize,
}

impl Emitter {
    pub(crate) fn new(categories: Vec<Emit>) -> Self {
        Self {
            categories,
            ..Self::default()
        }
    }

    pub(crate) fn reset(&mut self) {
        self.assistant_bytes = 0;
        self.args_bytes = 0;
        self.output_bytes = 0;
    }

    fn enabled(&self, category: Emit) -> bool {
        self.categories.contains(&Emit::All) || self.categories.contains(&category)
    }

    fn write(value: serde_json::Value) -> std::io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        serde_json::to_writer(&mut stderr, &value)?;
        stderr.write_all(b"\n")?;
        stderr.flush()
    }

    pub(crate) fn response(
        &mut self,
        turn: u32,
        round: usize,
        reply: &crate::adapter::LlmResult,
        secrets: &[String],
    ) -> std::io::Result<()> {
        for (category, kind, text) in [
            (Emit::Thinking, "thinking", &reply.thinking),
            (Emit::Text, "assistant_text", &reply.text),
        ] {
            if self.enabled(category) && !text.is_empty() {
                let text = bounded(
                    text,
                    secrets,
                    &mut self.assistant_bytes,
                    crate::limits::MAX_TURN_ASSISTANT_BYTES,
                );
                Self::write(json!({"type":kind,"turn":turn,"round":round,"text":text}))?;
            }
        }
        if self.enabled(Emit::Calls) {
            for (index, call) in reply.calls.iter().enumerate() {
                let args = bounded(
                    &call.args_json,
                    secrets,
                    &mut self.args_bytes,
                    crate::limits::MAX_TURN_ARGS_BYTES,
                );
                Self::write(
                    json!({"type":"tool_call","turn":turn,"round":round,"id":call.id,"name":call.name,"args":args,"index":index,"of":reply.calls.len()}),
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn result(
        &mut self,
        turn: u32,
        round: usize,
        call: &crate::session::ToolCallRecord,
        error_prefix: &str,
    ) -> std::io::Result<()> {
        if self.enabled(Emit::Results) {
            let result = if call.ok {
                call.result.clone()
            } else {
                format!("{error_prefix}{}", call.result)
            };
            // This record is constructed at model-request ingress, after scrubbing and caps.
            // Do not read a durable projection here: it may differ from the live result.
            self.output_bytes = self
                .output_bytes
                .checked_add(result.len())
                .ok_or_else(|| std::io::Error::other("transcript output size overflow"))?;
            if self.output_bytes > crate::limits::MAX_TURN_OUTPUT_BYTES {
                return Err(std::io::Error::other("transcript output exceeds turn cap"));
            }
            Self::write(
                json!({"type":"tool_result","turn":turn,"round":round,"id":call.id,"ok":call.ok,"refused":call.refused,"result":result}),
            )?;
        }
        Ok(())
    }
}

fn bounded(text: &str, secrets: &[String], used: &mut usize, cap: usize) -> String {
    let text = crate::redact::scrub_secrets(text, secrets);
    let text = crate::redact::truncate_utf8(text, cap.saturating_sub(*used));
    *used += text.len();
    text
}
