use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Consumer {
    ClassifierAssist,
    Compactor,
    RequiredFactExtractor,
    AssertionExtractor,
    EntailmentScorer,
    ReadBackQuerier,
}
impl Consumer {
    pub const fn all() -> [Self; 6] {
        [
            Self::ClassifierAssist,
            Self::Compactor,
            Self::RequiredFactExtractor,
            Self::AssertionExtractor,
            Self::EntailmentScorer,
            Self::ReadBackQuerier,
        ]
    }
    pub fn gate_only(self) -> bool {
        matches!(
            self,
            Self::RequiredFactExtractor | Self::AssertionExtractor | Self::EntailmentScorer
        )
    }
    pub fn on_failure(self) -> Disposition {
        match self {
            Self::ClassifierAssist => Disposition::StructuralAssignment,
            Self::Compactor => Disposition::DeterministicEmergency,
            Self::RequiredFactExtractor => Disposition::DeterministicRequiredUnion,
            Self::AssertionExtractor | Self::EntailmentScorer => Disposition::GateClosed,
            Self::ReadBackQuerier => Disposition::RangePagination,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    StructuralAssignment,
    DeterministicEmergency,
    DeterministicRequiredUnion,
    GateClosed,
    RangePagination,
    Queued,
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManagementError {
    #[error("invalid management spend configuration")]
    Configuration,
    #[error("management spend unavailable")]
    Exhausted,
    #[error("management intent, fence, or response failed")]
    Gateway,
    #[error("management data exceeds declared bounds")]
    Bounds,
    #[error("generated data failed derivation ingestion")]
    Ingestion,
    #[error("stale turn or outstanding gate reservation")]
    Turn,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotedEnvelope {
    pub instruction: String,
    /// JSON string values are quoted data; no untrusted preamble interpolation.
    pub quoted_data: Vec<String>,
    pub derivative_authority: String,
}
impl QuotedEnvelope {
    pub fn new(instruction: &str, data: &[String]) -> Self {
        Self {
            instruction: instruction.to_owned(),
            quoted_data: data.to_vec(),
            derivative_authority: "none".into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallIntent {
    pub identity: String,
    pub epoch: u64,
    pub turn: u64,
    pub consumer: Consumer,
    pub configuration: String,
    pub bound: u64,
    pub envelope: QuotedEnvelope,
}
/// Gateway owns durable identity, intent-before-send, fence and late suppression.
/// No retry method exists here: a non-idempotent identity gets one attempt.
pub trait ManagementGateway {
    fn record_intent(&mut self, intent: &CallIntent) -> Result<(), ManagementError>;
    fn send_once(&mut self, intent: &CallIntent) -> Result<String, ManagementError>;
    fn admit_response(&mut self, intent: &CallIntent, raw: &str) -> Result<(), ManagementError>;
}

impl ManagementError {
    pub fn disposition(&self, consumer: Consumer) -> Disposition {
        if *self == Self::Exhausted && !consumer.gate_only() {
            Disposition::Queued
        } else {
            consumer.on_failure()
        }
    }
}
