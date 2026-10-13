//! Epoch-granular cache accounting. There is no priced rewrite operation.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutTrigger {
    BudgetPressure,
    ExplicitRequest,
    /// Correctness-only: no invalidation price is assigned to erasure.
    Erasure,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochRecord {
    pub epoch: u64,
    pub head_digest: String,
    pub head_requests: usize,
    pub trigger: Option<CutTrigger>,
    pub armed: bool,
    pub bytes_reclaimed: u64,
    pub bytes_invalidated: u64,
    pub tokens_invalidated: Option<u64>,
    pub tokens_reclaimed: Option<u64>,
    pub invalidations: u64,
    pub aggregate_valid: bool,
    pub calls: u64,
    pub measured_calls: u64,
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub armed_input_tokens: u64,
    pub armed_cached_tokens: u64,
    pub disarmed_input_tokens: u64,
    pub disarmed_cached_tokens: u64,
}

impl EpochRecord {
    fn new(epoch: u64, head_digest: String, head_requests: usize) -> Self {
        Self {
            epoch,
            head_digest,
            head_requests,
            trigger: None,
            armed: false,
            bytes_reclaimed: 0,
            bytes_invalidated: 0,
            tokens_invalidated: None,
            tokens_reclaimed: None,
            invalidations: 0,
            aggregate_valid: true,
            calls: 0,
            measured_calls: 0,
            input_tokens: 0,
            cached_tokens: 0,
            armed_input_tokens: 0,
            armed_cached_tokens: 0,
            disarmed_input_tokens: 0,
            disarmed_cached_tokens: 0,
        }
    }
    pub fn hit_rate(&self) -> Option<f64> {
        self.aggregate_valid
            .then(|| ratio(self.cached_tokens, self.input_tokens))
            .flatten()
    }
    pub fn armed_hit_rate(&self) -> Option<f64> {
        self.aggregate_valid
            .then(|| ratio(self.armed_cached_tokens, self.armed_input_tokens))
            .flatten()
    }
    pub fn disarmed_hit_rate(&self) -> Option<f64> {
        self.aggregate_valid
            .then(|| ratio(self.disarmed_cached_tokens, self.disarmed_input_tokens))
            .flatten()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochCache {
    records: Vec<EpochRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EpochError {
    #[error("context epoch is unknown")]
    UnknownEpoch,
    #[error("context epoch counter exhausted")]
    Exhausted,
    #[error("context epoch accounting is corrupt")]
    Corrupt,
    #[error("context checkpoint refused by the governor")]
    GovernorRefused,
    #[error("context prefix changed without a checkpoint")]
    PrefixChanged,
}

impl EpochCache {
    pub fn records(&self) -> &[EpochRecord] {
        &self.records
    }

    pub fn start(&mut self, digest: String, requests: usize) -> Result<u64, EpochError> {
        let epoch = match self.records.last() {
            Some(last) => last.epoch.checked_add(1).ok_or(EpochError::Exhausted)?,
            None => 0,
        };
        self.records.push(EpochRecord::new(epoch, digest, requests));
        Ok(epoch)
    }

    pub fn cut(&mut self, cut: CheckpointCut) -> Result<u64, EpochError> {
        let epoch = self.start(cut.head_digest, cut.head_requests)?;
        let record = self.records.last_mut().ok_or(EpochError::UnknownEpoch)?;
        record.trigger = Some(cut.trigger);
        record.armed = cut.armed;
        record.bytes_reclaimed = cut.bytes_reclaimed;
        record.bytes_invalidated = cut.bytes_invalidated;
        record.tokens_invalidated = if cut.trigger == CutTrigger::Erasure {
            None
        } else {
            cut.tokens_invalidated
        };
        record.invalidations = 1;
        Ok(epoch)
    }

    /// Only normalized provider usage counts as a cache observation. Missing or
    /// inconsistent telemetry is a coverage gap, never a fabricated cache miss.
    pub fn observe(
        &mut self,
        epoch: u64,
        input: Option<u64>,
        cached: Option<u64>,
        armed: bool,
    ) -> Result<(), EpochError> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.epoch == epoch)
            .ok_or(EpochError::UnknownEpoch)?;
        record.calls = record.calls.checked_add(1).ok_or(EpochError::Exhausted)?;
        let Some((input, cached)) = input.zip(cached).filter(|(i, c)| c <= i) else {
            return Ok(());
        };
        record.measured_calls = record
            .measured_calls
            .checked_add(1)
            .ok_or(EpochError::Exhausted)?;
        if !record.aggregate_valid {
            return Ok(());
        }
        // Stage every total. Provider telemetry overflow invalidates metrics,
        // not the turn or prefix authority, and never persists partial totals.
        let mut staged = record.clone();
        if add_usage(&mut staged, input, cached, armed).is_err() {
            record.aggregate_valid = false;
        } else {
            *record = staged;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), EpochError> {
        for (index, record) in self.records.iter().enumerate() {
            if record.epoch != index as u64
                || record.head_digest.len() != 64
                || !record.head_digest.bytes().all(|b| b.is_ascii_hexdigit())
                || record.invalidations != u64::from(record.trigger.is_some())
                || record.cached_tokens > record.input_tokens
                || record.measured_calls > record.calls
                || record.armed_cached_tokens > record.armed_input_tokens
                || record.disarmed_cached_tokens > record.disarmed_input_tokens
                || record
                    .armed_input_tokens
                    .checked_add(record.disarmed_input_tokens)
                    != Some(record.input_tokens)
                || record
                    .armed_cached_tokens
                    .checked_add(record.disarmed_cached_tokens)
                    != Some(record.cached_tokens)
            {
                return Err(EpochError::Corrupt);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CheckpointCut {
    pub head_digest: String,
    pub head_requests: usize,
    pub trigger: CutTrigger,
    pub armed: bool,
    pub bytes_reclaimed: u64,
    pub bytes_invalidated: u64,
    pub tokens_invalidated: Option<u64>,
}

fn add_usage(
    record: &mut EpochRecord,
    input: u64,
    cached: u64,
    armed: bool,
) -> Result<(), EpochError> {
    add(&mut record.input_tokens, input)?;
    add(&mut record.cached_tokens, cached)?;
    let (total, hits) = if armed {
        (
            &mut record.armed_input_tokens,
            &mut record.armed_cached_tokens,
        )
    } else {
        (
            &mut record.disarmed_input_tokens,
            &mut record.disarmed_cached_tokens,
        )
    };
    add(total, input)?;
    add(hits, cached)
}
fn add(value: &mut u64, increment: u64) -> Result<(), EpochError> {
    *value = value.checked_add(increment).ok_or(EpochError::Exhausted)?;
    Ok(())
}
fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}
