//! Recorded semantic projection: only the transaction/reducer owns publication.
use super::*;
use crate::context_kernel::{canonical::Sink, lanes::Lane, scopes::ScopeRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticOperation {
    Note,
    ReadBack,
    Reopen,
    Fold,
    Compact,
    Condense,
    Regenerate,
}
impl SemanticOperation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::ReadBack => "read-back",
            Self::Reopen => "reopen",
            Self::Fold => "fold",
            Self::Compact => "compact",
            Self::Condense => "condense",
            Self::Regenerate => "regenerate",
        }
    }
    pub fn lossy(self) -> bool {
        matches!(
            self,
            Self::Fold | Self::Compact | Self::Condense | Self::Regenerate
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticCommand {
    pub operation: SemanticOperation,
    pub key: String,
    /// Input projection identities, not claimed pre-reclaim counts.
    pub sources: Vec<String>,
    /// Stable lane code avoids changing the kernel's existing wire vocabulary.
    pub lane: u8,
    pub candidate: Candidate,
    pub store_handle: String,
    pub authority: Option<crate::context_kernel::security::LaneAuthority>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticArtifact {
    pub command: SemanticCommand,
    pub validation: ValidationRecord,
    pub rendered: bool,
    pub depth: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticEvent {
    Put {
        command: Box<SemanticCommand>,
        validation: Box<ValidationRecord>,
    },
    FlushNotes,
    Reclassify {
        key: String,
        lane: u8,
    },
    /// Byte-exact split; children inherit support but can never become leaves.
    Resegment {
        key: String,
        parts: Vec<String>,
    },
}
impl SemanticEvent {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Put { command, .. } => command.operation.name(),
            Self::FlushNotes => "flush-notes",
            Self::Reclassify { .. } => "reclassify",
            Self::Resegment { .. } => "resegment",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticState {
    artifacts: BTreeMap<String, SemanticArtifact>,
}
impl SemanticState {
    pub fn artifacts(&self) -> &BTreeMap<String, SemanticArtifact> {
        &self.artifacts
    }
    pub fn rendered_bytes(&self) -> u64 {
        self.artifacts
            .values()
            .filter(|a| a.rendered)
            .map(|a| a.command.candidate.artifact.len() as u64)
            .sum()
    }
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("semantic-projection");
        sink.blob(&serde_json::to_vec(&self.artifacts).expect("integer semantic projection"));
    }
    pub(crate) fn apply(
        &mut self,
        event: &SemanticEvent,
        scopes: &ScopeRegistry,
    ) -> Result<(), GateError> {
        let mut next = self.clone();
        match event {
            SemanticEvent::Put {
                command,
                validation,
            } => next.put(command, validation, scopes)?,
            SemanticEvent::FlushNotes => {
                for artifact in next.artifacts.values_mut() {
                    artifact.rendered = true;
                }
            }
            SemanticEvent::Reclassify { key, lane } => {
                let artifact = next.artifacts.get_mut(key).ok_or(GateError::Bounds)?;
                // Classifier assist may only decrease protection; promotion is separately owned.
                if *lane < artifact.command.lane || *lane > 4 {
                    return Err(GateError::Bounds);
                }
                artifact.command.lane = *lane;
            }
            SemanticEvent::Resegment { key, parts } => next.split(key, parts)?,
        }
        *self = next;
        Ok(())
    }
    /// Erasure overlay removes every dependent derivative, including retired sources.
    pub(crate) fn erase(&mut self, leaf: &str) {
        for artifact in self.artifacts.values_mut() {
            if artifact
                .validation
                .support
                .values()
                .any(|s| s.contains(leaf))
            {
                artifact.command.candidate.artifact = "[erased: unverified tombstone]".into();
                artifact.command.candidate.rendered_text =
                    artifact.command.candidate.artifact.clone();
                artifact.command.candidate.rendered.clear();
                artifact.validation.support.clear();
                artifact.validation.candidate_digest =
                    candidate_digest(&artifact.command.candidate);
                artifact.validation.verdict =
                    Verdict::FailProvenance(ProvenanceFailure::UngroundedRoot);
            }
        }
    }
    fn put(
        &mut self,
        command: &SemanticCommand,
        validation: &ValidationRecord,
        scopes: &ScopeRegistry,
    ) -> Result<(), GateError> {
        if self.artifacts.contains_key(&command.key)
            || command.key.is_empty()
            || command.key.len() > 256
            || command.store_handle.is_empty()
            || validation.verdict != Verdict::Pass
            || validation.candidate_digest != candidate_digest(&command.candidate)
            || !(1..=4).contains(&command.lane)
            || self.artifacts.len() >= MAX_GRAPH_NODES
        {
            return Err(GateError::Bounds);
        }
        replay_authority(command, validation)?;
        let mut depth = 0;
        if command.operation.lossy() {
            if command.sources.is_empty() || command.lane == 1 {
                return Err(GateError::Bounds);
            }
            for key in &command.sources {
                let source = self.artifacts.get(key).ok_or(GateError::Bounds)?;
                if !source.rendered
                    || source.command.lane != command.lane
                    || source.command.candidate.scope != command.candidate.scope
                {
                    return Err(GateError::Bounds);
                }
                depth = depth.max(source.depth);
            }
            if command.operation == SemanticOperation::Fold
                && scopes
                    .state(command.candidate.scope)
                    .map_err(|_| GateError::Bounds)?
                    .is_open()
            {
                return Err(GateError::Bounds);
            }
            depth = if command.operation == SemanticOperation::Regenerate {
                0
            } else {
                depth.checked_add(1).ok_or(GateError::Bounds)?
            };
        } else if !command.sources.is_empty() {
            return Err(GateError::Bounds);
        }
        if command
            .sources
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != command.sources.len()
        {
            return Err(GateError::Bounds);
        }
        for key in &command.sources {
            self.artifacts.remove(key);
        }
        self.artifacts.insert(
            command.key.clone(),
            SemanticArtifact {
                command: command.clone(),
                validation: validation.clone(),
                rendered: command.operation != SemanticOperation::Note,
                depth,
            },
        );
        Ok(())
    }
    fn split(&mut self, key: &str, parts: &[String]) -> Result<(), GateError> {
        let original = self.artifacts.get(key).ok_or(GateError::Bounds)?.clone();
        if parts.len() < 2
            || parts.len() > MAX_ASSERTIONS
            || parts.iter().any(String::is_empty)
            || parts.concat() != original.command.candidate.artifact
            || self.artifacts.len() + parts.len() > MAX_GRAPH_NODES
        {
            return Err(GateError::Bounds);
        }
        self.artifacts.remove(key);
        for (index, text) in parts.iter().enumerate() {
            let id = format!("{key}/{index}");
            if self.artifacts.contains_key(&id) {
                return Err(GateError::Bounds);
            }
            let mut child = original.clone();
            child.command.key = id.clone();
            child.command.candidate.artifact = text.clone();
            child.validation.candidate_digest = candidate_digest(&child.command.candidate);
            self.artifacts.insert(id, child);
        }
        Ok(())
    }
}
pub fn lane_code(lane: Lane) -> u8 {
    match lane {
        Lane::Constitutional => 1,
        Lane::Decisional => 2,
        Lane::Evidential => 3,
        Lane::Ephemeral => 4,
    }
}

fn replay_authority(
    command: &SemanticCommand,
    validation: &ValidationRecord,
) -> Result<(), GateError> {
    if command.lane <= 2
        && !command.authority.as_ref().is_some_and(|a| {
            a.replay_valid(
                command.lane,
                command.candidate.scope,
                &validation.candidate_digest,
            )
        })
    {
        return Err(GateError::Bounds);
    }
    Ok(())
}
