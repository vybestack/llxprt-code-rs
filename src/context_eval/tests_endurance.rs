use super::endurance::*;
fn run(session: &str, restarted: bool) -> Run {
    let hour = 3_600_000;
    let intervals = if restarted {
        vec![
            (0, 12 * hour),
            (12 * hour + 1_000, 24 * hour + 1_000),
            (24 * hour + 2_000, 36 * hour + 2_000),
            (36 * hour + 3_000, 48 * hour + 3_000),
        ]
    } else {
        vec![(0, 48 * hour + 3_000)]
    };
    Run {
        session: session.into(),
        started_unix_ms: 0,
        finished_unix_ms: 48 * hour + 3_000,
        intervals: intervals
            .iter()
            .map(|&(start_ms, end_ms)| Interval {
                start_ms,
                end_ms,
                observations_digest: "a".repeat(64),
            })
            .collect(),
        restarts: if restarted {
            (0..3)
                .map(|i| Restart {
                    killed_unix_ms: (i + 1) * 12 * hour + i * 1_000,
                    resumed_unix_ms: (i + 1) * 12 * hour + (i + 1) * 1_000,
                    before_state_digest: "b".repeat(64),
                    after_state_digest: "b".repeat(64),
                    process_before: 100 + i as u32,
                    process_after: 200 + i as u32,
                })
                .collect()
        } else {
            vec![]
        },
        partial_credit_bp: 9_000,
        bounded_continuations: 100,
        continuations: 100,
        counts: Counts {
            out_of_branch_wall_hits: 0,
            unquiesced_armed_noops: 0,
            infeasibility_branches: 0,
            wrap_ups: 0,
            quiesces: 0,
            degraded_mode_entries: 2,
            governor_activations: 3,
        },
    }
}
fn evidence() -> Endurance {
    Endurance {
        schema_version: 1,
        candidate_sha: "c".repeat(40),
        protocol_digest: "d".repeat(64),
        workload_digest: "e".repeat(64),
        provider_class: "exact-replay".into(),
        restart_run: run("restarted", true),
        matched_no_restart: run("control", false),
    }
}
#[test]
fn endurance_fixture_qualifies_only_at_exact_head_and_protocol() {
    let e = evidence();
    assert!(e.validate(&"c".repeat(40), &"d".repeat(64)).is_ok());
    assert!(e.validate(&"f".repeat(40), &"d".repeat(64)).is_err());
    assert!(e.validate(&"c".repeat(40), &"f".repeat(64)).is_err());
}
#[test]
fn endurance_rejects_each_unmeasured_or_failed_release_condition() {
    for axis in 0..13 {
        let mut e = evidence();
        match axis {
            0 => e.restart_run.intervals[3].end_ms -= 1,
            1 => {
                e.restart_run.restarts.pop();
            }
            2 => e.restart_run.restarts[1].before_state_digest = "f".repeat(64),
            3 => e.restart_run.restarts[0].process_after = e.restart_run.restarts[0].process_before,
            4 => e.restart_run.intervals[0].end_ms += 1,
            5 => e.restart_run.intervals[1].start_ms -= 1,
            6 => e.restart_run.bounded_continuations = 98,
            7 => e.restart_run.counts.out_of_branch_wall_hits = 1,
            8 => e.restart_run.counts.unquiesced_armed_noops = 1,
            9 => e.restart_run.partial_credit_bp = 8_499,
            10 => e.matched_no_restart.session = e.restart_run.session.clone(),
            11 => e.matched_no_restart.restarts = e.restart_run.restarts.clone(),
            _ => e.restart_run.continuations = 0,
        }
        assert!(
            e.validate(&"c".repeat(40), &"d".repeat(64)).is_err(),
            "axis {axis}"
        );
    }
}
#[test]
fn short_restart_screen_cannot_impersonate_48_hour_endurance() {
    let mut e = evidence();
    e.restart_run.intervals = vec![Interval {
        start_ms: 0,
        end_ms: 1_000,
        observations_digest: "a".repeat(64),
    }];
    assert!(e
        .validate(&"c".repeat(40), &"d".repeat(64))
        .unwrap_err()
        .contains("48 recorded"));
}
