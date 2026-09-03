//! RED tests for the Phase-4 policy plane contracts.

use crate::context_policy::cache::{CacheConfig, RewriteEntry, RewriteJournal};
use crate::context_policy::governor::{Admission, Governor, GovernorConfig};
use crate::context_policy::ladder::{
    escalate, select, Candidate, Capabilities, LadderChoice, Rung,
};
use crate::context_policy::monitor::{MonitorSignal, RuntimeMonitor};
use crate::context_policy::params::{all, lookup, ParameterClass, PARAMETERS};
use crate::context_policy::pressure::{Pressure, SafetyTier, Thresholds};
use crate::context_policy::progress::{
    lexicographically_decreases, next_action, terminal_reserve, Macrostep, ProgressState,
    TerminalOutcome,
};
use crate::context_policy::queue::{
    find_admissible, ClassedQueues, OperationClass, Proposal, QueueClass, QueueConfig, SemanticKey,
    SourceRange,
};

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn key(operation: &'static str, start: u64, version: u64) -> SemanticKey {
    let operation = OperationClass::from_name(operation).expect("test operation is registered");
    SemanticKey::new(operation, vec![SourceRange::new(start, 8)], version)
}

fn proposal(
    class: QueueClass,
    operation: &'static str,
    start: u64,
    version: u64,
    tokens: u64,
) -> Proposal {
    Proposal::new(class, key(operation, start, version), tokens)
}

fn queues() -> ClassedQueues {
    ClassedQueues::new(QueueConfig::default())
}

#[test]
fn queue_reserved_shares_prevent_monitor_starvation() {
    let mut q = queues();
    for i in 0..8u64 {
        let accepted = q.submit(proposal(QueueClass::Model, "fold", 100 + i, 1, 64));
        assert!(accepted);
    }
    let monitor_accepted = q.submit(proposal(QueueClass::Monitor, "rule-update", 900, 1, 16));
    assert!(monitor_accepted);

    let serviced = q.service_cycle();
    let saw_monitor = serviced
        .iter()
        .any(|p| matches!(p.service_class, QueueClass::Monitor));
    let nonempty = !serviced.is_empty();
    assert!(nonempty);
    assert!(saw_monitor);
}

#[test]
fn semantic_dedup_and_bounded_reposal() {
    let mut q = queues();
    let original = proposal(QueueClass::Model, "fold", 10, 3, 32);
    assert!(q.submit(original.clone()));
    assert!(!q.submit(original.clone()));

    let other_operation = proposal(QueueClass::Model, "compact", 10, 3, 32);
    assert!(q.submit(other_operation));
    let other_service_class = proposal(QueueClass::Controller, "fold", 10, 3, 32);
    assert!(!q.submit(other_service_class));
    let newer_source = proposal(QueueClass::Model, "fold", 10, 4, 32);
    assert!(q.submit(newer_source));

    let serviced = q.service_cycle();
    let original_served = serviced.iter().any(|item| item.key == original.key);
    assert!(original_served);
    let bound = QueueConfig::default().max_retries;
    for _ in 0..bound {
        assert!(q.repropose(original.clone()));
        let cycle = q.service_cycle();
        assert!(!cycle.is_empty());
    }
    assert!(!q.repropose(original.clone()));
    assert_eq!(q.retries_for(&original.key), bound);
}

#[test]
fn find_admissible_is_registry_ordered_and_bounded() {
    let registry = crate::context_txn::operation::registry();
    assert_eq!(registry[0].name, "admit-ingress");
    let admissible = |row: &crate::context_txn::operation::Operation| {
        matches!(row.name, "placeholder-collapse" | "drop-with-handle")
    };
    let first = find_admissible(registry.len(), admissible);
    assert_eq!(first.map(|row| row.name), Some("placeholder-collapse"));
    let before_reclamation = find_admissible(13, admissible);
    assert!(before_reclamation.is_none());
    assert!(find_admissible(0, |_| true).is_none());
}

#[test]
fn governor_quota_forces_handle() {
    let cfg = GovernorConfig::new(100, 100, 1.0, 10);
    let mut gov = Governor::new(cfg);
    let early = gov.admit(7, 40, 1);
    let early_admit = matches!(early, Admission::Admit);
    assert!(early_admit);

    let late = gov.admit(7, 80, 1);
    let late_handle = matches!(late, Admission::Handle);
    assert!(late_handle);

    let other_source = gov.admit(9, 40, 1);
    let other_admit = matches!(other_source, Admission::Admit);
    assert!(other_admit);
}

#[test]
fn governor_violation_tightens_floor_then_quiesces() {
    let cfg = GovernorConfig::new(1000, 1000, 1.0, 100);
    let mut gov = Governor::new(cfg);
    let admitted = gov.admit(1, 1000, 1);
    let was_admitted = matches!(admitted, Admission::Admit);
    assert!(was_admitted);

    gov.observe_reclaim(100, 1);
    let quota_after_violation = gov.state().quota;
    let tightened = quota_after_violation < 1000;
    assert!(tightened);
    let not_yet_at_floor = gov.state().quota > 100;
    assert!(not_yet_at_floor);

    gov.observe_reclaim(100, 1);
    let at_floor = gov.state().at_floor;
    assert!(at_floor);
    let quiescing = gov.state().quiescing;
    assert!(quiescing);
    let next = gov.admit(1, 1, 1);
    let quiesced = matches!(next, Admission::Quiesce);
    assert!(quiesced);
}

#[test]
fn pressure_hysteresis_reaches_effective_target() {
    let bad_order = Thresholds::new(0.7, 0.8, 0.9);
    let rejected = bad_order.is_err();
    assert!(rejected);

    let th = Thresholds::new(0.8, 0.7, 0.6).unwrap();
    let target_below_disarm = th.target < th.disarm;
    assert!(target_below_disarm);

    let mut p = Pressure::new(th);
    let armed_tier = p.observe(0.9, 0.9, 0.5);
    let armed = matches!(armed_tier, SafetyTier::Armed);
    assert!(armed);

    let effective = p.effective_target();
    let targets_floor = near(effective, 0.6);
    assert!(targets_floor);

    p.observe(0.95, 0.95, 0.75);
    let raised = p.effective_target();
    let targets_minimum = near(raised, 0.75);
    assert!(targets_minimum);

    let disarmed_tier = p.observe(0.5, 0.5, 0.75);
    let disarmed = matches!(disarmed_tier, SafetyTier::Disarmed);
    assert!(disarmed);
}

#[test]
fn ladder_fixed_order_and_capability_adjustment() {
    let caps_off = Capabilities {
        collapse_placeholders: false,
        drop_with_handle: true,
    };
    let only_collapse = [Candidate::new(Rung::CollapsePlaceholders, 500.0, 0.9)];
    let chosen = select(&only_collapse, &caps_off, false, 0.5);
    let avoided = !matches!(chosen, LadderChoice::Rung(Rung::CollapsePlaceholders));
    assert!(avoided);

    let caps_on = Capabilities::default();
    let allowed = select(&only_collapse, &caps_on, false, 0.5);
    let permitted = matches!(allowed, LadderChoice::Rung(Rung::CollapsePlaceholders));
    assert!(permitted);

    let ladder = Rung::all();
    let ordered = ladder[0] == Rung::FoldAwayEphemeral;
    assert!(ordered);
    let terminal_rung = ladder[5] == Rung::Condense;
    assert!(terminal_rung);

    let capped = escalate(3, 3);
    let wraps = matches!(capped, LadderChoice::WrapUp);
    assert!(wraps);
    let zero_bound = escalate(0, 0);
    let quiesces = matches!(zero_bound, LadderChoice::Quiesce);
    assert!(quiesces);
}

#[test]
fn scorer_outage_uses_deterministic_emergency_set() {
    let caps = Capabilities::default();
    let candidates = [
        Candidate::new(Rung::Condense, 9000.0, 0.10),
        Candidate::new(Rung::FoldAwayEphemeral, 1.0, 0.05),
    ];
    let chosen = select(&candidates, &caps, false, 0.5);
    let fell_back = matches!(chosen, LadderChoice::Rung(Rung::FoldAwayEphemeral));
    assert!(fell_back);

    let empty = Vec::new();
    let nothing = select(&empty, &caps, true, 0.5);
    let terminal = matches!(nothing, LadderChoice::Quiesce);
    assert!(terminal);
}

#[test]
fn monitor_sticky_caps_freeze_and_relax_only_disarmed() {
    let mut m = RuntimeMonitor::new(2);
    m.observe(MonitorSignal::Thrash, true);
    m.observe(MonitorSignal::Thrash, true);
    m.observe(MonitorSignal::Thrash, true);
    let capped = m.counter(MonitorSignal::Thrash);
    let at_cap = capped == 2;
    assert!(at_cap);

    m.fail();
    let frozen_before = m.counter(MonitorSignal::Reacquisition);
    m.observe(MonitorSignal::Reacquisition, true);
    let frozen_after = m.counter(MonitorSignal::Reacquisition);
    let froze = frozen_before == frozen_after;
    assert!(froze);

    let early = m.proposals(0);
    let early_relaxation = early.iter().any(|p| p.relax_filter);
    assert!(!early_relaxation);

    let late = m.proposals(1);
    let late_relaxation = late.iter().any(|p| p.relax_filter);
    assert!(late_relaxation);
}

#[test]
fn cache_threshold_boundaries_and_unknown_cost() {
    let cfg = CacheConfig {
        amortization_bar: 100,
        flush_epoch: 4,
    };
    let mut journal = RewriteJournal::new(cfg);

    let below = journal.should_rewrite(100, Some(50), false);
    assert!(!below);

    let boundary = journal.should_rewrite(150, Some(50), false);
    assert!(boundary);

    let just_under = journal.should_rewrite(149, Some(50), false);
    assert!(!just_under);

    // Unknown invalidation cost stays None; it is never treated as zero.
    let unknown = journal.should_rewrite(10, None, false);
    assert!(!unknown);

    let report = journal.report();
    let denials = report.threshold_denials > 0;
    let passes = report.threshold_passes > 0;
    let accounted = denials;
    let observed_passes = passes;
    assert!(accounted);
    assert!(observed_passes);
    let unknowns = journal.report().unknown_cost_events == 0;
    assert!(unknowns);
}

#[test]
fn cache_forced_flush_and_armed_suspension() {
    let cfg = CacheConfig {
        amortization_bar: 100,
        flush_epoch: 4,
    };
    let mut journal = RewriteJournal::new(cfg);

    // Armed pressure suspends economics entirely.
    let armed_rewrite = journal.should_rewrite(10, Some(5000), true);
    assert!(armed_rewrite);

    journal.note(RewriteEntry::new(50, Some(5), 7));
    let early_flush = journal.should_flush();
    assert!(!early_flush);
    journal.note(RewriteEntry::new(60, Some(6), 8));
    journal.note(RewriteEntry::new(70, Some(7), 9));
    journal.note(RewriteEntry::new(80, Some(8), 10));
    let epoch_ready = journal.should_flush();
    assert!(epoch_ready);

    let flushed = journal.force_flush(3, true, 42);
    let nonempty = !flushed.is_empty();
    assert!(nonempty);
    let unamortized = flushed.iter().all(|e| e.amortized);
    assert!(!unamortized);
    let timed = flushed.iter().any(|e| e.wall_elapsed_us == 42);
    assert!(timed);
    let forced = journal.report().forced_flushes == 1;
    assert!(forced);
}

#[test]
fn parameters_are_total_and_classed_once() {
    let expected = [
        "safety.classification_floor",
        "safety.mandatory_reserve",
        "queue.cycle_slots",
        "queue.monitor_share",
        "queue.max_retries",
        "governor.per_window_quota",
        "governor.per_turn_ceiling",
        "governor.alpha",
        "governor.quota_floor",
        "pressure.arm",
        "pressure.disarm",
        "pressure.target",
        "pressure.minimum_floor",
        "ladder.amortization_bar",
        "ladder.escalation_bound",
        "ladder.confidence_floor",
        "monitor.sticky_cap",
        "monitor.relaxation_windows",
        "cache.amortization_bar",
        "cache.flush_epoch",
        "cache.invalidation_penalty",
        "progress.mandatory_queue_weight",
        "progress.retry_weight",
        "progress.terminal_reserve",
        "profile.reclaim_aggressiveness",
        "profile.log_verbosity",
    ];

    let mut names = std::collections::BTreeSet::new();
    let mut total = true;
    for name in expected.iter() {
        match lookup(name) {
            Some(param) => {
                let inserted = names.insert(param.name);
                if !inserted {
                    total = false;
                }
            }
            None => total = false,
        }
    }
    assert!(total);
    let no_aliases = names.len() == PARAMETERS.len();
    assert!(no_aliases);
    let complete = names.len() == expected.len();
    assert!(complete);

    let expected_classes = [
        ParameterClass::SafetyInvariant,
        ParameterClass::SafetyInvariant,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::Calibrated,
        ParameterClass::OperatorEnvelope,
        ParameterClass::SafetyInvariant,
        ParameterClass::OperatorEnvelope,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::OperatorEnvelope,
        ParameterClass::ProfileTunable,
        ParameterClass::OperatorEnvelope,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::Calibrated,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
        ParameterClass::SafetyInvariant,
        ParameterClass::ProfileTunable,
        ParameterClass::ProfileTunable,
    ];
    assert_eq!(all().len(), expected_classes.len());
    for (parameter, expected_class) in all().iter().zip(expected_classes) {
        assert_eq!(
            parameter.class, expected_class,
            "wrong class for {}",
            parameter.name
        );
    }
    assert!(!crate::context_policy::params::calibratable_while_armed(
        "governor.per_window_quota"
    ));
    assert!(crate::context_policy::params::calibratable_while_armed(
        "pressure.arm"
    ));

    let classes: Vec<ParameterClass> = all().iter().map(|p| p.class).collect();
    let has_safety = classes
        .iter()
        .any(|c| matches!(c, ParameterClass::SafetyInvariant));
    let has_profile = classes
        .iter()
        .any(|c| matches!(c, ParameterClass::ProfileTunable));
    let has_calibrated = classes
        .iter()
        .any(|c| matches!(c, ParameterClass::Calibrated));
    let has_envelope = classes
        .iter()
        .any(|c| matches!(c, ParameterClass::OperatorEnvelope));
    let covered = has_safety;
    let tuned = has_profile;
    let measured = has_calibrated;
    let bounded = has_envelope;
    assert!(covered);
    assert!(tuned);
    assert!(measured);
    assert!(bounded);
}

#[test]
fn macrostep_measure_decreases_or_retries_decrease() {
    let before = ProgressState::new(5, 2);
    let psi_drop = ProgressState::new(3, 3);
    let psi_improved = lexicographically_decreases(before, psi_drop);
    assert!(psi_improved);

    let retries_drop = ProgressState::new(5, 1);
    let retries_improved = lexicographically_decreases(before, retries_drop);
    assert!(retries_improved);

    let worse = ProgressState::new(6, 3);
    let regressed = lexicographically_decreases(before, worse);
    assert!(!regressed);

    let stalled = lexicographically_decreases(before, before);
    assert!(!stalled);
}

#[test]
fn reachable_armed_states_have_terminal_or_reclaim_action() {
    let state = ProgressState::new(4, 2);
    let armed_step = next_action(state, true, None);
    let armed_noop = matches!(armed_step, Macrostep::NoOp);
    assert!(!armed_noop);

    let armed_terminal = next_action(state, true, Some(TerminalOutcome::WrapUp));
    let armed_terminal_noop = matches!(armed_terminal, Macrostep::NoOp);
    assert!(!armed_terminal_noop);

    let disarmed_step = next_action(state, false, None);
    let disarmed_noop = matches!(disarmed_step, Macrostep::NoOp);
    assert!(!disarmed_noop);
}

#[test]
fn terminal_reserve_wrap_up_is_feasible() {
    let infeasible = terminal_reserve(10, 5);
    assert!(!infeasible);

    let feasible = terminal_reserve(5, 10);
    assert!(feasible);

    let exact = terminal_reserve(5, 5);
    assert!(exact);
}
