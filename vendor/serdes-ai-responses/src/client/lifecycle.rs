//! Per-turn tool lifecycle integrity, checked before wire evidence is discarded.
//!
//! The live Codex backend sends two shapes that the strict loopback fixtures did
//! not model, and both are accepted here:
//! - a call may stream no argument deltas at all and carry its arguments only in
//!   `function_call_arguments.done` (and the matching `output_item.done`);
//! - the terminal response may carry `output: []` even though tool items streamed.
//!
//! Arguments from `done` are used only when no delta arrived. When deltas did
//! arrive, `done` and the item must match their exact bytes. A terminal output
//! that does list a tool item must still agree with the streamed item; omission is
//! not a contradiction, but disagreement is.
use crate::types::{OutputItem, OutputItemStatus, ResponseObject, StreamEvent};
use serdes_ai_models::ModelError;
use std::collections::BTreeMap;

fn invalid(reason: &str) -> ModelError {
    ModelError::InvalidResponse(format!(
        "responses tool lifecycle: {reason}; request not replayed"
    ))
}

struct Item {
    start: OutputItem,
    arguments: String,
    saw_delta: bool,
    arguments_done: bool,
    ended: bool,
}

#[derive(Default)]
pub(super) struct Lifecycle {
    items: BTreeMap<u64, Item>,
}

impl Lifecycle {
    /// Check one wire event. When a call supplied its arguments only in
    /// `function_call_arguments.done`, returns the equivalent argument delta that
    /// must be translated immediately before `event`, because translation
    /// otherwise drops `done` and the call would reach the consumer empty.
    pub(super) fn validate(
        &mut self,
        event: &StreamEvent,
    ) -> Result<Option<StreamEvent>, ModelError> {
        let mut adopted = None;
        match event {
            StreamEvent::OutputItemAdded {
                output_index, item, ..
            } => {
                self.start(*output_index, item)?;
            }
            StreamEvent::FunctionCallArgumentsDelta {
                output_index,
                item_id,
                delta,
                ..
            } => {
                let item = self.active_call(*output_index, item_id)?;
                if item.arguments_done {
                    return Err(invalid("argument delta after arguments done"));
                }
                item.arguments.push_str(delta);
                item.saw_delta = true;
            }
            StreamEvent::FunctionCallArgumentsDone {
                sequence_number,
                output_index,
                item_id,
                arguments,
            } => {
                let item = self.active_call(*output_index, item_id)?;
                if item.arguments_done || (item.saw_delta && item.arguments != *arguments) {
                    return Err(invalid("duplicate or contradictory arguments done"));
                }
                item.arguments_done = true;
                if !item.saw_delta && !arguments.is_empty() {
                    item.arguments.clone_from(arguments);
                    adopted = Some(StreamEvent::FunctionCallArgumentsDelta {
                        sequence_number: *sequence_number,
                        output_index: *output_index,
                        item_id: item_id.clone(),
                        delta: arguments.clone(),
                    });
                }
            }
            StreamEvent::OutputItemDone {
                output_index, item, ..
            } => {
                self.end(*output_index, item)?;
            }
            StreamEvent::ResponseCompleted { response, .. }
            | StreamEvent::ResponseIncomplete { response, .. } => self.complete(response)?,
            _ => {}
        }
        Ok(adopted)
    }

    fn start(&mut self, index: u64, start: &OutputItem) -> Result<(), ModelError> {
        if index != self.items.len() as u64
            || self.items.contains_key(&index)
            || self
                .items
                .values()
                .any(|item| item.start.item_id() == start.item_id())
            || start.item_id().is_empty()
        {
            return Err(invalid(
                "noncontiguous, duplicate or empty output item identity",
            ));
        }
        if let OutputItem::FunctionCall {
            arguments, status, ..
        } = start
        {
            if !arguments.is_empty() || *status != OutputItemStatus::InProgress {
                return Err(invalid("tool start is not an empty in-progress item"));
            }
        }
        self.items.insert(
            index,
            Item {
                start: start.clone(),
                arguments: String::new(),
                saw_delta: false,
                arguments_done: false,
                ended: false,
            },
        );
        Ok(())
    }

    fn active_call(&mut self, index: u64, id: &str) -> Result<&mut Item, ModelError> {
        let item = self
            .items
            .get_mut(&index)
            .ok_or_else(|| invalid("arguments for unknown output index"))?;
        if item.start.item_id() != id || !matches!(item.start, OutputItem::FunctionCall { .. }) {
            return Err(invalid("argument item/index identity mismatch"));
        }
        if item.ended {
            return Err(invalid("arguments after output item done"));
        }
        Ok(item)
    }

    fn end(&mut self, index: u64, done: &OutputItem) -> Result<(), ModelError> {
        let item = self
            .items
            .get_mut(&index)
            .ok_or_else(|| invalid("done for unknown output index"))?;
        if item.ended
            || item.start.item_id() != done.item_id()
            || std::mem::discriminant(&item.start) != std::mem::discriminant(done)
        {
            return Err(invalid("duplicate or mismatched output item done"));
        }
        if matches!(item.start, OutputItem::FunctionCall { .. }) {
            if !item.arguments_done {
                return Err(invalid("tool item done before arguments done"));
            }
            item.corroborate(done)?;
        }
        item.ended = true;
        Ok(())
    }

    fn complete(&self, response: &ResponseObject) -> Result<(), ModelError> {
        for (index, item) in &self.items {
            if matches!(item.start, OutputItem::FunctionCall { .. }) {
                if !item.arguments_done || !item.ended {
                    return Err(invalid("terminal response before tool completion"));
                }
                // An omitted terminal item is the live wire shape, not evidence
                // against the stream; a present one must agree with it.
                if let Some(final_item) = usize::try_from(*index)
                    .ok()
                    .and_then(|index| response.output.get(index))
                {
                    item.corroborate(final_item)?;
                }
            }
        }
        for (index, output) in response.output.iter().enumerate() {
            if matches!(output, OutputItem::FunctionCall { .. }) {
                let item = self
                    .items
                    .get(&(index as u64))
                    .ok_or_else(|| invalid("terminal response has unstarted tool item"))?;
                item.corroborate(output)?;
            }
        }
        Ok(())
    }
}

impl Item {
    fn corroborate(&self, done: &OutputItem) -> Result<(), ModelError> {
        match (&self.start, done) {
            (
                OutputItem::FunctionCall {
                    id, call_id, name, ..
                },
                OutputItem::FunctionCall {
                    id: done_id,
                    call_id: done_call_id,
                    name: done_name,
                    arguments,
                    status,
                },
            ) if id == done_id
                && call_id == done_call_id
                && name == done_name
                && self.arguments == *arguments
                && *status == OutputItemStatus::Completed =>
            {
                Ok(())
            }
            _ => Err(invalid("contradictory tool completion evidence")),
        }
    }
}
