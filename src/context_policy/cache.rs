//! Rewrite journal: threshold economics, flush epochs, forced flush, reporting.

/// One rewrite journal record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RewriteEntry {
    pub tokens_reclaimed: u64,
    pub invalidation_cost: Option<u64>,
    pub logical_time: u64,
    pub wall_elapsed_us: u64,
    pub amortized: bool,
}

impl RewriteEntry {
    pub const fn new(
        tokens_reclaimed: u64,
        invalidation_cost: Option<u64>,
        logical_time: u64,
    ) -> Self {
        Self {
            tokens_reclaimed,
            invalidation_cost,
            logical_time,
            wall_elapsed_us: 0,
            amortized: true,
        }
    }
}

/// Rewrite economics.
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

/// Reported cache behavior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheReport {
    pub hits: u64,
    pub events: u64,
    pub invalidation_total: u64,
    pub unknown_cost_events: u64,
    pub threshold_passes: u64,
    pub threshold_denials: u64,
    pub forced_flushes: u64,
    pub armed_rewrites: u64,
    pub hit_rate: f64,
}

impl Default for CacheReport {
    fn default() -> Self {
        Self {
            hits: 0,
            events: 0,
            invalidation_total: 0,
            unknown_cost_events: 0,
            threshold_passes: 0,
            threshold_denials: 0,
            forced_flushes: 0,
            armed_rewrites: 0,
            hit_rate: 0.0,
        }
    }
}

/// Rewrite journal with amortization-aware admission.
#[derive(Debug)]
pub struct RewriteJournal {
    config: CacheConfig,
    entries: Vec<RewriteEntry>,
    noted: Vec<RewriteEntry>,
    report: CacheReport,
}

impl RewriteJournal {
    pub fn new(config: CacheConfig) -> Self {
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

    /// Record a rewrite that happened (journal accounting).
    pub fn record(&mut self, entry: RewriteEntry) {
        self.report.events += 1;
        if let Some(cost) = entry.invalidation_cost {
            self.report.invalidation_total += cost;
        } else {
            self.report.unknown_cost_events += 1;
        }
        self.entries.push(entry);
    }

    /// Rewrite is allowed when expected benefit >= invalidation cost +
    /// amortization bar. Armed pressure suspends economics entirely.
    pub fn should_rewrite(
        &mut self,
        expected_benefit: u64,
        invalidation_cost: Option<u64>,
        armed: bool,
    ) -> bool {
        // RED: ignores the amortization bar and unknown-cost conservatism, and
        // does not suspend economics while armed.
        let _ = armed;
        self.report.events += 1;
        match invalidation_cost {
            Some(cost) => expected_benefit >= cost,
            None => true,
        }
    }

    /// Epoch-batched note flush.
    pub fn should_flush(&self) -> bool {
        // RED: flushes early.
        !self.noted.is_empty()
    }

    /// Note an entry for a later epoch batch.
    pub fn note(&mut self, entry: RewriteEntry) {
        self.noted.push(entry);
    }

    /// Forced flush before a lossy operation touching a noted source.
    pub fn force_flush(
        &mut self,
        source: u64,
        armed: bool,
        wall_elapsed_us: u64,
    ) -> Vec<RewriteEntry> {
        // RED: reports forced flushes as amortized even while armed and never
        // registers the wall elapsed time.
        let _ = (source, wall_elapsed_us);
        self.report.forced_flushes += 1;
        if armed {
            self.report.armed_rewrites += 1;
        }
        let mut drained = std::mem::take(&mut self.noted);
        for entry in drained.iter_mut() {
            entry.amortized = false;
            entry.wall_elapsed_us = wall_elapsed_us;
        }
        self.entries.extend_from_slice(&drained);
        drained
    }

    pub fn report(&self) -> &CacheReport {
        &self.report
    }
}
