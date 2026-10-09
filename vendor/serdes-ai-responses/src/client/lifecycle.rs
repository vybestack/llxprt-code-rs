//! Per-turn tool lifecycle integrity, checked before wire evidence is discarded.
//! Completion objects corroborate delta bytes; they never supply or repair them.
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
    arguments_done: bool,
    ended: bool,
}

#[derive(Default)]
pub(super) struct Lifecycle {
    items: BTreeMap<u64, Item>,
}

impl Lifecycle {
    pub(super) fn validate(&mut self, event: &StreamEvent) -> Result<(), ModelError> {
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
            }
            StreamEvent::FunctionCallArgumentsDone {
                output_index,
                item_id,
                arguments,
                ..
            } => {
                let item = self.active_call(*output_index, item_id)?;
                if item.arguments_done || item.arguments != *arguments {
                    return Err(invalid("duplicate or contradictory arguments done"));
                }
                item.arguments_done = true;
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
        Ok(())
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
                let final_item = usize::try_from(*index)
                    .ok()
                    .and_then(|index| response.output.get(index))
                    .ok_or_else(|| invalid("terminal response missing tool item"))?;
                item.corroborate(final_item)?;
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
