//! RED tests for the Phase-4 policy plane contracts.

use crate::context_policy::cache::{CacheConfig, RewriteEntry, RewriteJournal};
use crate::context_policy::governor::{Admission, Governor, GovernorConfig};
use crate::context_policy::ladder::{
    escalate, operation, select, select_candidate, Candidate, Capabilities, LadderChoice, Rung,
};
use crate::context_policy::monitor::{MonitorSignal, RuntimeMonitor};
use crate::context_policy::params::{
    all, lookup, ParameterClass, ParameterRegistry, UpdateAuthority, UpdateError, PARAMETERS,
};
use crate::context_policy::pressure::{Pressure, SafetyTier, Thresholds};
use crate::context_policy::progress::{
    lexicographically_decreases, next_action, operation_for_degradation, terminal_reserve,
    verify_adversarial_reachable_states, DegradationModes, Macrostep, ProgressState,
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
fn semantic_dedup_and_bounded_reproposal() {
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
    let first_reclamation = registry
        .iter()
        .position(|row| row.name == "placeholder-collapse")
        .expect("reclamation operation is registered");
    let before_reclamation = find_admissible(first_reclamation, admissible);
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
    assert!(matches!(gov.admit(1, 1000, 1), Admission::Admit));
    gov.observe_reclaim(100, 1);
    assert!(!gov.finish_window(1));
    assert!(gov.state().quota < 1000);
    assert!(!gov.state().quiescing);

    for window in 2..=32 {
        gov.begin_turn();
        let quota = gov.state().quota;
        assert!(matches!(gov.admit(1, quota, window), Admission::Admit));
        gov.observe_reclaim(1, window);
        assert!(!gov.finish_window(window));
        if gov.state().quiescing {
            break;
        }
    }
    assert!(gov.state().at_floor);
    assert!(gov.state().quiescing);
    assert!(matches!(gov.admit(1, 1, 33), Admission::Quiesce));
}

#[test]
fn governor_turn_ceiling_resets_without_resetting_window_quota() {
    let mut gov = Governor::new(GovernorConfig::new(100, 60, 1.0, 10));
    assert!(matches!(gov.admit(1, 60, 1), Admission::Admit));
    assert!(matches!(gov.admit(2, 1, 1), Admission::Handle));
    gov.begin_turn();
    assert!(matches!(gov.admit(1, 40, 1), Admission::Admit));
    assert!(matches!(gov.admit(1, 1, 1), Admission::Handle));
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
    let chosen = select(&only_collapse, &caps_off, true, 0.5);
    let avoided = !matches!(chosen, LadderChoice::Rung(Rung::CollapsePlaceholders));
    assert!(avoided);

    let caps_on = Capabilities::default();
    let allowed = select(&only_collapse, &caps_on, true, 0.5);
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
    let fell_back = matches!(chosen, LadderChoice::Emergency(Rung::FoldAwayEphemeral));
    assert!(fell_back);

    let empty = Vec::new();
    let nothing = select(&empty, &caps, false, 0.5);
    let terminal = matches!(nothing, LadderChoice::Quiesce);
    assert!(terminal);
}

#[test]
fn monitor_sticky_caps_freeze_and_relax_only_disarmed() {
    let mut m = RuntimeMonitor::new(2);
    m.observe(MonitorSignal::Thrash);
    m.observe(MonitorSignal::Thrash);
    m.observe(MonitorSignal::Thrash);
    let capped = m.counter(MonitorSignal::Thrash);
    let at_cap = capped == 2;
    assert!(at_cap);

    m.fail();
    let frozen_before = m.counter(MonitorSignal::Reacquisition);
    m.observe(MonitorSignal::Reacquisition);
    let frozen_after = m.counter(MonitorSignal::Reacquisition);
    let froze = frozen_before == frozen_after;
    assert!(froze);

    m.begin_window(true);
    let early = m.proposals();
    let early_relaxation = early.iter().any(|p| p.relax_filter);
    assert!(!early_relaxation);

    m.begin_window(false);
    let late = m.proposals();
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

    journal.note(RewriteEntry::new(3, 50, Some(5), 7));
    let early_flush = journal.should_flush();
    assert!(!early_flush);
    journal.note(RewriteEntry::new(3, 60, Some(6), 8));
    journal.note(RewriteEntry::new(3, 70, Some(7), 9));
    journal.note(RewriteEntry::new(3, 80, Some(8), 10));
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
    journal.observe_access(true, true);
    journal.observe_access(false, false);
    assert_eq!(journal.report().hit_rate, Some(0.5));
    assert_eq!(journal.report().armed_hit_rate, Some(1.0));
    assert_eq!(journal.report().disarmed_hit_rate, Some(0.0));
    assert_eq!(journal.report().invalidation_cost_per_event, Some(6.5));
    assert_eq!(journal.report().economic_gate_suspensions, 1);
    journal.record(RewriteEntry::new(3, 1, None, 11));
    assert_eq!(journal.report().invalidation_cost_per_event, None);
}

const EXPECTED_PARAMETERS: [(&str, ParameterClass); 26] = [
    (
        "safety.classification_floor",
        ParameterClass::SafetyInvariant,
    ),
    ("safety.mandatory_reserve", ParameterClass::SafetyInvariant),
    ("queue.cycle_slots", ParameterClass::ProfileTunable),
    ("queue.monitor_share", ParameterClass::ProfileTunable),
    ("queue.max_retries", ParameterClass::ProfileTunable),
    ("governor.per_window_quota", ParameterClass::Calibrated),
    (
        "governor.per_turn_ceiling",
        ParameterClass::OperatorEnvelope,
    ),
    ("governor.alpha", ParameterClass::SafetyInvariant),
    ("governor.quota_floor", ParameterClass::OperatorEnvelope),
    ("pressure.arm", ParameterClass::ProfileTunable),
    ("pressure.disarm", ParameterClass::ProfileTunable),
    ("pressure.target", ParameterClass::ProfileTunable),
    ("pressure.minimum_floor", ParameterClass::OperatorEnvelope),
    ("ladder.amortization_bar", ParameterClass::ProfileTunable),
    ("ladder.escalation_bound", ParameterClass::OperatorEnvelope),
    ("ladder.confidence_floor", ParameterClass::ProfileTunable),
    ("monitor.sticky_cap", ParameterClass::ProfileTunable),
    ("monitor.relaxation_windows", ParameterClass::ProfileTunable),
    ("cache.amortization_bar", ParameterClass::ProfileTunable),
    ("cache.flush_epoch", ParameterClass::ProfileTunable),
    ("cache.invalidation_penalty", ParameterClass::Calibrated),
    (
        "progress.mandatory_queue_weight",
        ParameterClass::ProfileTunable,
    ),
    ("progress.retry_weight", ParameterClass::ProfileTunable),
    ("progress.terminal_reserve", ParameterClass::SafetyInvariant),
    (
        "profile.reclaim_aggressiveness",
        ParameterClass::ProfileTunable,
    ),
    ("profile.log_verbosity", ParameterClass::ProfileTunable),
];

#[test]
fn parameters_are_total_and_classed_once() {
    assert_eq!(all().len(), EXPECTED_PARAMETERS.len());
    assert_eq!(PARAMETERS.len(), EXPECTED_PARAMETERS.len());
    let mut names = std::collections::BTreeSet::new();
    for (parameter, (name, class)) in all().iter().zip(EXPECTED_PARAMETERS) {
        assert_eq!(parameter.name, name);
        assert_eq!(parameter.class, class, "wrong class for {name}");
        assert!(names.insert(parameter.name), "duplicate parameter {name}");
        assert_eq!(
            lookup(name).map(|found| (found.name, found.class)),
            Some((parameter.name, parameter.class)),
        );
    }
    for class in [
        ParameterClass::SafetyInvariant,
        ParameterClass::ProfileTunable,
        ParameterClass::Calibrated,
        ParameterClass::OperatorEnvelope,
    ] {
        assert!(all().iter().any(|parameter| parameter.class == class));
    }
    assert!(!crate::context_policy::params::calibratable_while_armed(
        "governor.per_window_quota"
    ));
    assert!(crate::context_policy::params::calibratable_while_armed(
        "pressure.arm"
    ));
}

#[test]
fn constructor_positive_parameters_reject_zero_updates() {
    let cases = [
        ("cache.flush_epoch", UpdateAuthority::ProfileLoad),
        ("governor.per_window_quota", UpdateAuthority::CalibrationTxn),
        ("governor.per_turn_ceiling", UpdateAuthority::Operator),
        ("governor.quota_floor", UpdateAuthority::Operator),
    ];
    for (name, authority) in cases {
        let mut registry = ParameterRegistry::default();
        let result = registry.update(name, 0.0, 1, authority, false);
        assert_eq!(
            result,
            Err(UpdateError::InvalidValue),
            "accepted zero for {name}"
        );
    }
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
fn every_degradation_axis_changes_the_selected_registered_operation() {
    let cases = [
        (1, ProgressState::new(2, 2)),
        (2, ProgressState::new(2, 2)),
        (4, ProgressState::new(2, 2)),
        (8, ProgressState::new(2, 2)),
        (16, ProgressState::new(1, 2)),
        (32, ProgressState::new(2, 2)),
        (64, ProgressState::new(2, 2)),
    ];
    for (bits, state) in cases {
        let baseline = operation_for_degradation(state, DegradationModes::from_bits(0));
        let operation = operation_for_degradation(state, DegradationModes::from_bits(bits));
        assert_ne!(
            operation, baseline,
            "degradation bit {bits} was behaviorally inert"
        );
        assert!(operation
            .and_then(crate::context_txn::operation::find)
            .is_some());
    }
}

#[test]
fn runtime_bulk_accounting_uses_measured_pressure_without_panicking() {
    let mut policy = crate::context_policy::runtime::ProposalOnlyController::default();
    let bytes = vec![b'x'; 2048];
    let proposal = policy.propose_bulk("read_file", bytes.len(), 1.0);
    assert!(proposal.armed);
    policy.complete_bulk(proposal, &bytes, 5000, 0.2, 7);
    assert_eq!(
        policy.terminal_outcome(),
        None,
        "an ordinary disarmed completion records no terminal"
    );
    assert_eq!(policy.cache_report().economic_gate_suspensions, 1);
    assert!(!policy.events()[0].armed_after);
    policy.wrap_up(0, u64::MAX, true);
    assert_eq!(policy.terminal_outcome(), Some("wrap_up"));
}

#[test]
fn runtime_failed_proposal_quiesces_and_wrap_up_cannot_override_it() {
    let mut policy = crate::context_policy::runtime::ProposalOnlyController::default();
    let proposal = policy.propose_bulk("read_file", 2048, 1.0);
    policy.abort_bulk(proposal);
    assert_eq!(policy.terminal_outcome(), Some("quiesce_unwritable"));
    assert_eq!(policy.events()[0].operation, "quiesce-unwritable");
    policy.wrap_up(0, u64::MAX, true);
    assert_eq!(policy.terminal_outcome(), Some("quiesce_unwritable"));
}

/// Drives the controller's own governor into a rate quiesce the way
/// production does: every window admits at the quota while reclaiming one
/// byte, so `predicate_holds` keeps failing, the quota tightens to the floor,
/// and the governor quiesces (`governor_violation_tightens_floor_then_quiesces`
/// proves the same walk on the bare governor). The completion after that is
/// refused by the quiescing quota itself, which is what records the rate
/// terminal as its own event instead of a write failure.
fn drive_governor_to_rate_quiesce(
    policy: &mut crate::context_policy::runtime::ProposalOnlyController,
) {
    let mut window = 0;
    while !policy.governor().state().quiescing {
        window += 1;
        assert!(
            window < 64,
            "the governor must tighten to the floor and then quiesce"
        );
        let quota = policy.governor().state().quota;
        let bytes = vec![b'x'; quota as usize + 1];
        let proposal = policy.propose_bulk("read_file", bytes.len(), 1.0);
        policy.complete_bulk(proposal, &bytes, quota as usize, 1.0, 7);
    }
    assert!(policy.governor().state().at_floor);
    // The next admission is refused by the quiescing quota, so the caller never
    // touches the store on this path: the rate terminal is the quota's refusal.
    let bytes = vec![b'x'; 8];
    let proposal = policy.propose_bulk("read_file", bytes.len(), 1.0);
    policy.complete_bulk(proposal, &bytes, 8, 1.0, 7);
}

/// A rate quiesce and an unwritable quiesce are DIFFERENT terminals, and both
/// survive recovery: the rate branch is the quota's own refusal and the
/// unwritable branch is a store write failure, so a durable consumer can tell
/// them apart.
#[test]
fn rate_and_unwritable_quiesce_are_distinct_terminals_and_both_recover() {
    // Force the rate refusal the way production reaches it: each window admits
    // right at the quota while reclaiming a single byte, so the admission
    // predicate keeps failing, the quota walks down to the floor, and the
    // governor quiesces. The NEXT completion is then refused by the quota
    // itself, never by the store.
    let mut policy = crate::context_policy::runtime::ProposalOnlyController::default();
    drive_governor_to_rate_quiesce(&mut policy);
    assert_eq!(
        policy
            .events()
            .last()
            .expect("a quiesced completion is still an event")
            .operation,
        "quiesce-rate"
    );
    assert_ne!(
        policy.terminal_outcome(),
        Some("quiesce_unwritable"),
        "the rate terminal must differ from the write-failure terminal"
    );

    // Both terminals survive the recovery mapping unchanged.
    assert_eq!(
        crate::session::context_recover::recover_terminal_outcome(Some("quiesce_rate".to_string())),
        Some("quiesce_rate")
    );
    assert_eq!(
        crate::session::context_recover::recover_terminal_outcome(Some(
            "quiesce_unwritable".to_string()
        )),
        Some("quiesce_unwritable")
    );
    // A name nobody records is left unset rather than rewritten.
    assert_eq!(
        crate::session::context_recover::recover_terminal_outcome(Some("made-up".to_string())),
        None
    );

    // wrap_up refuses only the unwritable branch: a session that hit the rate
    // ceiling can still be finalized when the store is writable and the
    // wrap-up fits, which is what makes the two terminals honest.
    let mut rate = crate::context_policy::runtime::ProposalOnlyController::default();
    drive_governor_to_rate_quiesce(&mut rate);
    assert_eq!(rate.terminal_outcome(), Some("quiesce_rate"));
    rate.wrap_up(0, u64::MAX, true);
    assert_eq!(rate.terminal_outcome(), Some("wrap_up"));
    // The unwritable branch stays refused, as before.
    let mut wedged = crate::context_policy::runtime::ProposalOnlyController::default();
    let proposal = wedged.propose_bulk("read_file", 2048, 1.0);
    wedged.abort_bulk(proposal);
    wedged.wrap_up(0, u64::MAX, true);
    assert_eq!(wedged.terminal_outcome(), Some("quiesce_unwritable"));
    // A restored rate terminal is durable across the restore mapping itself:
    // the policy keeps it (it is not None and not rewritten), and a durable
    // consumer can tell it from `quiesce_unwritable`. `wrap_up` refuses only
    // the write-failure branch, so an explicit writable wrap-up is still
    // recorded on top of it - the split is about WHICH refusal happened, not
    // about wedging a writable store.
    let mut restored = crate::context_policy::runtime::ProposalOnlyController::default();
    restored.restore_terminal_outcome("quiesce_rate");
    assert_eq!(restored.terminal_outcome(), Some("quiesce_rate"));
    restored.wrap_up(0, u64::MAX, true);
    assert_eq!(restored.terminal_outcome(), Some("wrap_up"));
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

#[test]
fn adversarial_reachable_states_terminate_without_wall_or_armed_noop() {
    let report = verify_adversarial_reachable_states(6, 6, 16);
    assert_eq!(report.reachable_states, 128 * 7 * 7);
    assert_eq!(report.out_of_branch_wall_hits, 0);
    assert_eq!(report.armed_noops, 0);
    assert_eq!(report.unterminated_episodes, 0);
    assert!(report.max_macrosteps <= 16);
}

#[test]
fn estimator_reorders_only_inside_one_rung_and_emergency_is_registered() {
    let candidates = [
        Candidate::new(Rung::Fold, 10.0, 0.9),
        Candidate::new(Rung::Fold, 30.0, 0.9),
        Candidate::new(Rung::Compact, 900.0, 0.9),
    ];
    let selection = select_candidate(&candidates, &Capabilities::default(), true, 0.5);
    assert_eq!(selection.choice, LadderChoice::Rung(Rung::Fold));
    assert_eq!(selection.candidate_index, Some(1));
    let emergency = select_candidate(&candidates, &Capabilities::default(), false, 0.5);
    assert_eq!(emergency.choice, LadderChoice::Emergency(Rung::Fold));
    let registered = operation(emergency.choice).and_then(crate::context_txn::operation::find);
    assert!(registered.is_some());
}

#[test]
fn parameter_mutations_are_authorized_logged_and_armed_safe() {
    let mut registry = ParameterRegistry::default();
    assert_eq!(
        ParameterRegistry::class_of("unknown.parameter"),
        ParameterClass::SafetyInvariant
    );
    let safety = registry.update("governor.alpha", 0.4, 1, UpdateAuthority::Operator, false);
    assert_eq!(safety, Err(UpdateError::SafetyInvariant));
    let wrong = registry.update("pressure.arm", 0.9, 1, UpdateAuthority::Operator, false);
    assert_eq!(wrong, Err(UpdateError::WrongAuthority));
    let blocked = registry.update(
        "cache.invalidation_penalty",
        20.0,
        1,
        UpdateAuthority::CalibrationTxn,
        true,
    );
    assert_eq!(blocked, Err(UpdateError::ArmedCalibration));
    registry
        .update(
            "cache.invalidation_penalty",
            20.0,
            1,
            UpdateAuthority::CalibrationTxn,
            false,
        )
        .unwrap();
    registry
        .update(
            "governor.per_turn_ceiling",
            2048.0,
            2,
            UpdateAuthority::Operator,
            true,
        )
        .unwrap();
    assert_eq!(registry.updates().len(), 2);
    assert_eq!(registry.value("cache.invalidation_penalty"), Some(20.0));
}

#[test]
fn source_scoped_forced_flush_leaves_other_notes_pending() {
    let mut journal = RewriteJournal::new(CacheConfig {
        amortization_bar: 1,
        flush_epoch: 2,
    });
    journal.note(RewriteEntry::new(1, 10, Some(1), 1));
    journal.note(RewriteEntry::new(2, 20, Some(2), 2));
    let flushed = journal.force_flush(1, true, 9);
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].source, 1);
    assert_eq!(journal.len(), 1);
    let second = journal.force_flush(2, false, 10);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].source, 2);
}
