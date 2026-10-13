//! Exact-candidate semantic receipt boundary; callers cannot supply a passing verdict.
use super::{
    budget::Budget,
    executor::{CommitOutcome, Executor, ExecutorError, FencingClock},
};
use crate::{
    context_ingress::ingress::{IngressRecord, IngressSink, IngressTxn},
    context_kernel::{
        events::{EventKind, Sequencer},
        legality::is_legal,
        reducer::{Reducer, TypedState, IDLENESS_WINDOW},
        validation::*,
    },
};
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SemanticError {
    #[error("semantic gate rejected candidate")]
    Rejected(ValidationRecord),
    #[error("semantic validation unavailable")]
    Gate(GateError),
    #[error("semantic operation/ingress/receipt boundary mismatch")]
    Boundary,
    #[error("semantic replay rejected effect")]
    Replay(crate::context_kernel::reducer::ReducerError),
    #[error("semantic transaction refused")]
    Transaction(ExecutorError),
}
/// No public constructor: only gate validation plus successful derivation ingress mint this.
pub struct SemanticReceipt {
    event: SemanticEvent,
    bound: u64,
}
/// Read-only validation owners plus the existing derivation-ingestion capability.
pub struct SemanticServices<'a> {
    pub gate: &'a Gate,
    pub required: &'a [BoundFact],
    pub always_checked: &'a [BoundFact],
    pub store: &'a dyn LeafStore,
    pub consumers: &'a mut dyn GateConsumers,
    pub ingress: &'a mut IngressTxn,
    pub sink: &'a mut dyn IngressSink,
}
impl Executor {
    /// No governed publication before all three independent boundaries pass.
    pub fn validate_semantic(
        &mut self,
        mut command: SemanticCommand,
        services: SemanticServices<'_>,
        budget: &Budget,
    ) -> Result<SemanticReceipt, SemanticError> {
        let SemanticServices {
            gate,
            required,
            always_checked,
            store,
            consumers,
            ingress,
            sink,
        } = services;
        let result = (|| {
            self.check_semantic_operation(command.operation.name())?;
            let items = self
                .semantic()
                .artifacts()
                .values()
                .filter(|a| a.rendered && a.command.lane <= 2)
                .flat_map(|a| {
                    a.command.candidate.rendered.iter().cloned().map(move |f| {
                        (
                            if a.command.lane == 1 {
                                crate::context_kernel::lanes::Lane::Constitutional
                            } else {
                                crate::context_kernel::lanes::Lane::Decisional
                            },
                            f,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let floor = deterministic_required(
                &self.typed.ledger,
                &items,
                required,
                &self.typed.lane_policy_registry,
                command.candidate.scope,
            );
            let record = gate
                .validate(&command.candidate, &floor, always_checked, store, consumers)
                .map_err(SemanticError::Gate)?;
            if record.verdict != Verdict::Pass {
                return Err(SemanticError::Rejected(record));
            }
            ingress
                .capture_derivation(
                    command.operation.name(),
                    command.candidate.artifact.as_bytes(),
                )
                .map_err(|_| SemanticError::Boundary)?;
            let records = ingress.commit(sink).map_err(|_| SemanticError::Boundary)?;
            let admitted = exact_derivation(&records, &command.candidate.artifact)?;
            command.store_handle = admitted.payload.handle.clone();
            let event = SemanticEvent::Put {
                command: Box::new(command),
                validation: Box::new(record),
            };
            let bound = self.validate_semantic_event(&event, budget)?;
            self.semantic_validation = Some((event.clone(), bound));
            Ok(SemanticReceipt { event, bound })
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    /// Deterministic projection operations reuse the same universal transaction gates.
    pub fn validate_projection(
        &mut self,
        event: SemanticEvent,
        budget: &Budget,
    ) -> Result<SemanticReceipt, SemanticError> {
        let result = (|| {
            if matches!(event, SemanticEvent::Put { .. }) {
                return Err(SemanticError::Boundary);
            }
            self.check_semantic_operation(event.operation())?;
            let bound = self.validate_semantic_event(&event, budget)?;
            self.semantic_validation = Some((event.clone(), bound));
            Ok(SemanticReceipt { event, bound })
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn check_semantic_operation(&self, op: &str) -> Result<(), SemanticError> {
        let txn = self.current.as_ref().ok_or(SemanticError::Boundary)?;
        if txn.op != op || self.proposed_by.is_none() {
            return Err(SemanticError::Boundary);
        }
        Ok(())
    }
    fn semantic_candidate(&self, event: &SemanticEvent) -> Result<TypedState, SemanticError> {
        let mut log = self.log.clone();
        let mut writer = Sequencer::continuing(&log, self.epoch.0, 1_000);
        log.append(writer.append(
            EventKind::Semantic {
                event: Box::new(event.clone()),
            },
            log.store_version(),
        ))
        .map_err(|_| SemanticError::Boundary)?;
        let mut candidate = self.typed.clone();
        Reducer::new(IDLENESS_WINDOW)
            .fold_from(&mut candidate, &log)
            .map_err(SemanticError::Replay)?;
        Ok(candidate)
    }
    fn validate_semantic_event(
        &mut self,
        event: &SemanticEvent,
        budget: &Budget,
    ) -> Result<u64, SemanticError> {
        let candidate = self.semantic_candidate(event)?;
        let bytes = serde_json::to_vec(event)
            .map_err(|_| SemanticError::Boundary)?
            .len() as u64;
        let bound = self.port_bound(0, bytes).ok_or(SemanticError::Boundary)?;
        let before = self.typed.semantic.rendered_bytes();
        let after = candidate.semantic.rendered_bytes();
        let floor = candidate
            .ledger
            .entries()
            .filter(|e| {
                !e.retired && e.class == crate::context_kernel::ledger::FactClass::Obligation
            })
            .map(|e| e.fold.text.len() as u64)
            .sum();
        self.validate(bound, budget, floor, before, after, bytes)
            .map_err(SemanticError::Transaction)?;
        let row = super::operation::find(event.operation()).ok_or(SemanticError::Boundary)?;
        // Semantic outputs never inherit emergency exemptions: real net reclaim is mandatory.
        if !super::budget::fits(bound, budget)
            || (row.reclamation && before.saturating_sub(after) < row.bar)
        {
            return Err(SemanticError::Boundary);
        }
        Ok(bound)
    }
    pub fn commit_semantic(
        &mut self,
        actual_parent: u64,
        clock: &FencingClock,
        receipt: SemanticReceipt,
    ) -> Result<CommitOutcome, SemanticError> {
        let result = (|| {
            self.check_semantic_operation(receipt.event.operation())?;
            if clock.latest() > self.epoch.0 {
                return Err(SemanticError::Transaction(ExecutorError::Fenced {
                    held: clock.latest(),
                    mine: self.epoch.0,
                }));
            }
            if self.semantic_validation.as_ref() != Some(&(receipt.event.clone(), receipt.bound)) {
                return Err(SemanticError::Boundary);
            }
            let candidate = self.semantic_candidate(&receipt.event)?;
            is_legal(&candidate, &self.contract)
                .map_err(|v| SemanticError::Transaction(super::executor::illegality(v)))?;
            self.commit_checked(
                actual_parent,
                Some(EventKind::Semantic {
                    event: Box::new(receipt.event),
                }),
                receipt.bound,
            )
            .map_err(SemanticError::Transaction)
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    pub fn semantic(&self) -> &SemanticState {
        &self.typed.semantic
    }
}
fn exact_derivation<'a>(
    records: &'a [IngressRecord],
    artifact: &str,
) -> Result<&'a IngressRecord, SemanticError> {
    if records.len() != 1 {
        return Err(SemanticError::Boundary);
    }
    let record = &records[0];
    if record.source != crate::context_ingress::capture::CaptureSource::GeneratedArtifact
        || record.payload.bytes != artifact.as_bytes()
        || record.vault.is_some()
        || record.spine.is_none()
    {
        return Err(SemanticError::Boundary);
    }
    Ok(record)
}

impl SemanticError {
    pub fn response(&self) -> Response {
        match self {
            Self::Rejected(record) => record.verdict.response(),
            _ => Response::DeterministicEmergency,
        }
    }
}
