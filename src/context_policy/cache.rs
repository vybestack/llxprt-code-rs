//! Rewrite journal, amortization gates, flush epochs, and conditional cache reports.

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
        let _ = source;
        self.report.forced_flushes = self.report.forced_flushes.saturating_add(1);
        let mut drained = std::mem::take(&mut self.noted);
        for entry in &mut drained {
            entry.amortized = !armed;
            entry.wall_elapsed_us = wall_elapsed_us;
            self.record(*entry);
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
