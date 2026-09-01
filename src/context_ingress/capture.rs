//! Bounded volatile capture buffer for the ingress transaction.
//!
//! Every external or generated payload first lands here, un-redacted, in memory only.
//! The buffer is bounded, is never encoded into any durable artifact, and is drained
//! into the redactor inside the same transaction that performs the sanitized append.
//! Losing it to a crash is a declared loss: the sanitized spine simply does not contain
//! the payload yet, and replaying the source re-derives it deterministically.

/// Where a captured payload came from. Local to ingress so the capture buffer does not
/// couple to kernel event encoding details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    UserPrompt,
    ToolResult,
    ToolCall,
    Assistant,
    GeneratedArtifact,
    LegacyImport,
}

impl CaptureSource {
    /// Stable name used in sanitized record provenance.
    pub fn name(self) -> &'static str {
        match self {
            CaptureSource::UserPrompt => "user-prompt",
            CaptureSource::ToolResult => "tool-result",
            CaptureSource::ToolCall => "tool-call",
            CaptureSource::Assistant => "assistant",
            CaptureSource::GeneratedArtifact => "generated-artifact",
            CaptureSource::LegacyImport => "legacy-import",
        }
    }
}

/// One un-redacted payload awaiting redaction.
pub struct CaptureSlot {
    pub source: CaptureSource,
    pub bytes: Vec<u8>,
}

/// Declared loss semantics of the buffer if the process dies now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureLoss {
    /// Nothing is at risk: no un-redacted bytes are held.
    Empty,
    /// Un-redacted bytes are at risk and are lost on a crash.
    VolatileBytes(usize),
}

/// Errors raised by the capture buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The payload does not fit in the remaining capacity; ingress fails closed.
    CapacityExceeded { cap: usize, requested: usize },
}

/// Bounded, volatile, never-persisted capture buffer.
pub struct CaptureBuffer {
    cap: usize,
    slots: Vec<CaptureSlot>,
    at_risk: usize,
}

impl CaptureBuffer {
    /// Creates a buffer holding at most `cap` un-redacted bytes.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            slots: Vec::new(),
            at_risk: 0,
        }
    }

    /// Capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Un-redacted bytes currently held.
    pub fn at_risk(&self) -> usize {
        self.at_risk
    }

    /// True when no un-redacted bytes are held.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Declared loss if the process dies now.
    pub fn declared_loss(&self) -> CaptureLoss {
        if self.at_risk == 0 {
            CaptureLoss::Empty
        } else {
            CaptureLoss::VolatileBytes(self.at_risk)
        }
    }

    /// Captures one payload, failing closed on overflow.
    pub fn push(&mut self, source: CaptureSource, bytes: &[u8]) -> Result<(), CaptureError> {
        if self.at_risk + bytes.len() > self.cap {
            return Err(CaptureError::CapacityExceeded {
                cap: self.cap,
                requested: bytes.len(),
            });
        }
        self.at_risk += bytes.len();
        self.slots.push(CaptureSlot {
            source,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    /// Removes and returns every captured payload for redaction.
    pub fn drain(&mut self) -> Vec<CaptureSlot> {
        self.at_risk = 0;
        std::mem::take(&mut self.slots)
    }

    /// Simulates a crash: the buffer is lost and nothing durable changed.
    pub fn simulate_crash(&mut self) -> usize {
        let lost = self.at_risk;
        self.slots.clear();
        self.at_risk = 0;
        lost
    }
}
