//! Rewrite journal, amortization gates, flush epochs, and conditional cache reports.

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RewriteEntry {
    pub source: u64,
    /// Bytes this rewrite reclaimed, in budget units (renamed from the
    /// token-spelling `tokens_reclaimed`, issue 123). The alias keeps
    /// journals written by pre-rename builds recoverable: `load_rewrite_journal`
    /// treats a line that fails to deserialize as a corrupt artifact and fails
    /// the whole recovery, so without the alias a pre-rename session could
    /// never be reopened (issue 102 recovery).
    #[serde(alias = "tokens_reclaimed")]
    pub bytes_reclaimed: u64,
    pub invalidation_cost: Option<u64>,
    pub logical_time: u64,
    pub wall_elapsed_us: u64,
    pub amortized: bool,
}

impl RewriteEntry {
    pub const fn new(
        source: u64,
        bytes_reclaimed: u64,
        invalidation_cost: Option<u64>,
        logical_time: u64,
    ) -> Self {
        Self {
            source,
            bytes_reclaimed,
            invalidation_cost,
            logical_time,
            wall_elapsed_us: 0,
            amortized: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheConfig {
    pub amortization_bar: u64,
    pub flush_epoch: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            amortization_bar: 100,
            flush_epoch: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct CacheReport {
    pub hits: u64,
    pub events: u64,
    pub armed_hits: u64,
    pub armed_events: u64,
    pub disarmed_hits: u64,
    pub disarmed_events: u64,
    pub rewrite_events: u64,
    pub invalidation_total: u64,
    pub known_cost_events: u64,
    pub unknown_cost_events: u64,
    pub threshold_passes: u64,
    pub threshold_denials: u64,
    pub armed_threshold_passes: u64,
    pub disarmed_threshold_passes: u64,
    pub forced_flushes: u64,
    pub armed_rewrites: u64,
    pub disarmed_rewrites: u64,
    pub economic_gate_suspensions: u64,
    pub hit_rate: Option<f64>,
    pub armed_hit_rate: Option<f64>,
    pub disarmed_hit_rate: Option<f64>,
    pub invalidation_cost_per_event: Option<f64>,
}

#[derive(Debug)]
pub struct RewriteJournal {
    config: CacheConfig,
    entries: Vec<RewriteEntry>,
    noted: Vec<RewriteEntry>,
    report: CacheReport,
}

impl RewriteJournal {
    pub fn new(config: CacheConfig) -> Self {
        assert!(config.flush_epoch > 0, "flush epoch must be positive");
        Self {
            config,
            entries: Vec::new(),
            noted: Vec::new(),
            report: CacheReport::default(),
        }
    }

    pub fn config(&self) -> &CacheConfig {
        &self.config
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entries(&self) -> &[RewriteEntry] {
        &self.entries
    }

    /// Restores one durable journal entry a previous process recorded, so a
    /// republished journal carries the previous records ahead of the new ones
    /// instead of replacing them (issue 102 restart).
    ///
    /// The entry's own counters are folded into the running report the same
    /// way `record` folds a live entry, so a restored journal still yields a
    /// report a later publication can render faithfully.
    pub fn restore_entry(&mut self, entry: RewriteEntry) {
        let cost = entry.invalidation_cost;
        self.report.rewrite_events = self.report.rewrite_events.saturating_add(1);
        match cost {
            Some(cost) => {
                self.report.invalidation_total =
                    self.report.invalidation_total.saturating_add(cost);
                self.report.known_cost_events = self.report.known_cost_events.saturating_add(1);
            }
            None => {
                self.report.unknown_cost_events = self.report.unknown_cost_events.saturating_add(1);
            }
        }
        if entry.amortized {
            self.report.disarmed_rewrites = self.report.disarmed_rewrites.saturating_add(1);
        } else {
            self.report.armed_rewrites = self.report.armed_rewrites.saturating_add(1);
        }
        self.entries.push(entry);
        self.refresh_report();
    }

    pub fn observe_access(&mut self, hit: bool, armed: bool) {
        self.report.events = self.report.events.saturating_add(1);
        if hit {
            self.report.hits = self.report.hits.saturating_add(1);
        }
        if armed {
            self.report.armed_events = self.report.armed_events.saturating_add(1);
            if hit {
                self.report.armed_hits = self.report.armed_hits.saturating_add(1);
            }
        } else {
            self.report.disarmed_events = self.report.disarmed_events.saturating_add(1);
            if hit {
                self.report.disarmed_hits = self.report.disarmed_hits.saturating_add(1);
            }
        }
        self.refresh_report();
    }

    pub fn record(&mut self, entry: RewriteEntry) {
        self.report.rewrite_events = self.report.rewrite_events.saturating_add(1);
        match entry.invalidation_cost {
            Some(cost) => {
                self.report.invalidation_total =
                    self.report.invalidation_total.saturating_add(cost);
                self.report.known_cost_events = self.report.known_cost_events.saturating_add(1);
            }
            None => {
                self.report.unknown_cost_events = self.report.unknown_cost_events.saturating_add(1)
            }
        }
        if entry.amortized {
            self.report.disarmed_rewrites = self.report.disarmed_rewrites.saturating_add(1);
        } else {
            self.report.armed_rewrites = self.report.armed_rewrites.saturating_add(1);
        }
        self.entries.push(entry);
        self.refresh_report();
    }

    pub fn should_rewrite(
        &mut self,
        expected_benefit: u64,
        invalidation_cost: Option<u64>,
        armed: bool,
    ) -> bool {
        let allowed = armed
            || invalidation_cost.is_some_and(|cost| {
                expected_benefit >= cost.saturating_add(self.config.amortization_bar)
            });
        if armed {
            self.report.economic_gate_suspensions =
                self.report.economic_gate_suspensions.saturating_add(1);
        }
        if allowed {
            self.report.threshold_passes = self.report.threshold_passes.saturating_add(1);
            if armed {
                self.report.armed_threshold_passes =
                    self.report.armed_threshold_passes.saturating_add(1);
            } else {
                self.report.disarmed_threshold_passes =
                    self.report.disarmed_threshold_passes.saturating_add(1);
            }
        } else {
            self.report.threshold_denials = self.report.threshold_denials.saturating_add(1);
        }
        allowed
    }

    pub fn should_flush(&self) -> bool {
        self.noted.len() >= self.config.flush_epoch
    }
    pub fn note(&mut self, entry: RewriteEntry) {
        self.noted.push(entry);
    }

    pub fn force_flush(
        &mut self,
        source: u64,
        armed: bool,
        wall_elapsed_us: u64,
    ) -> Vec<RewriteEntry> {
        self.report.forced_flushes = self.report.forced_flushes.saturating_add(1);
        let noted = std::mem::take(&mut self.noted);
        let mut drained = Vec::new();
        for mut entry in noted {
            if entry.source == source {
                entry.amortized = !armed;
                entry.wall_elapsed_us = wall_elapsed_us;
                self.record(entry);
                drained.push(entry);
            } else {
                self.noted.push(entry);
            }
        }
        drained
    }

    pub fn report(&self) -> &CacheReport {
        &self.report
    }

    fn refresh_report(&mut self) {
        self.report.hit_rate = ratio(self.report.hits, self.report.events);
        self.report.armed_hit_rate = ratio(self.report.armed_hits, self.report.armed_events);
        self.report.disarmed_hit_rate =
            ratio(self.report.disarmed_hits, self.report.disarmed_events);
        self.report.invalidation_cost_per_event =
            if self.report.rewrite_events == 0 || self.report.unknown_cost_events > 0 {
                None
            } else {
                ratio(self.report.invalidation_total, self.report.rewrite_events)
            };
    }
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue 121-c (F13): a journal line written by a pre-rename build - which
    /// spelled the field `tokens_reclaimed` - must still deserialize, because
    /// `load_rewrite_journal` fails the whole recovery on an entry that does
    /// not. The alias is the only thing keeping those sessions recoverable.
    #[test]
    fn a_pre_rename_journal_line_still_deserializes() {
        let line = r#"{"source":7,"tokens_reclaimed":4096,"invalidation_cost":null,"logical_time":3,"wall_elapsed_us":12,"amortized":true}"#;
        let entry: RewriteEntry =
            serde_json::from_str(line).expect("the old spelling must deserialize");
        assert_eq!(entry.bytes_reclaimed, 4096);
        // New builds write the new spelling, and both spellings never mix in
        // one entry: the new key is the one serialization emits.
        let round: String = serde_json::to_string(&entry).expect("entry serializes");
        assert!(round.contains("bytes_reclaimed"));
        assert!(!round.contains("tokens_reclaimed"));
    }

    /// A journal written by the CURRENT build keeps restoring through the same
    /// path recovery uses, so the alias never masks a real corruption of the
    /// new spelling.
    #[test]
    fn a_current_journal_line_still_deserializes() {
        let line = r#"{"source":9,"bytes_reclaimed":32,"invalidation_cost":8,"logical_time":5,"wall_elapsed_us":0,"amortized":false}"#;
        let entry: RewriteEntry =
            serde_json::from_str(line).expect("the new spelling must deserialize");
        assert_eq!(entry.bytes_reclaimed, 32);
        assert_eq!(entry.invalidation_cost, Some(8));
    }
}
