//! Vocabulary restore tests split out of the main ingress test module so both
//! files stay under the quality gate's 800 effective-LOC ceiling.
use super::*;

/// Snapshotting the vocabularies and restoring them twice (two restarts) is a
/// true round trip: the version count and labels are IDENTICAL to before,
/// `vocabulary_at(N)` still resolves for every recorded version, and no phantom
/// version is minted per restart. The old restore paired `snapshots[i]`'s version
/// with `snapshots[i-1]`'s labels and appended `snapshots.len()+1`, so every
/// restart grew the vocabulary version for an unchanged vocabulary and
/// `vocabulary_at(N>=2)` resolved the wrong labels (issue 118 regression).
#[test]
fn vocabulary_restore_survives_two_restarts_without_phantom_versions() {
    let mut registry = FilterRegistry::new();
    // v1 seeded, then one in-session addition so the history has two versions.
    let mut update = Vocabulary::v1();
    update.version = 2;
    update.labels = vec!["error-span", "identifier", "commit-hash"];
    assert_eq!(registry.update_vocabulary(update).unwrap(), 2);
    let before = registry.vocabulary_snapshots();
    let expected: Vec<(u64, Vec<String>)> = before
        .iter()
        .map(|snapshot| (snapshot.version, snapshot.labels.clone()))
        .collect();

    // Two restarts: each one snapshots the live registry and restores it.
    let mut current = before.clone();
    for _ in 0..2 {
        let mut restarted = FilterRegistry::new();
        restarted
            .restore_vocabulary_snapshots(current.clone())
            .expect("a legal additions-only history restores");
        current = restarted.vocabulary_snapshots();
        // The round trip is identity: same version count, same labels.
        let restored: Vec<(u64, Vec<String>)> = current
            .iter()
            .map(|snapshot| (snapshot.version, snapshot.labels.clone()))
            .collect();
        assert_eq!(
            restored, expected,
            "restore is a round trip: no phantom version, no shifted labels"
        );
        // Every recorded version still resolves under its OWN labels, not its
        // neighbour's: the old off-by-one paired `snapshots[i]`'s version with
        // `snapshots[i-1]`'s labels.
        for snapshot in &current {
            let vocabulary = restarted
                .vocabulary_at(snapshot.version)
                .expect("every recorded version keeps resolving");
            assert_eq!(vocabulary.version, snapshot.version);
            let labels: Vec<&str> = vocabulary.labels.to_vec();
            let expected_labels: Vec<&str> = snapshot.labels.iter().map(String::as_str).collect();
            assert_eq!(
                labels, expected_labels,
                "version {} resolves under its own labels",
                snapshot.version
            );
        }
    }
    // The version count never grows with restarts.
    assert_eq!(
        current.len(),
        before.len(),
        "two restarts do not mint a phantom version"
    );
    assert_eq!(
        current.last().unwrap().version,
        2,
        "the current version stays 2 across restarts"
    );
}

/// A snapshot that would drop a label is still a typed refusal, so the round
/// trip above is the ONLY legal restore shape.
#[test]
fn vocabulary_restore_refuses_a_label_drop() {
    let mut registry = FilterRegistry::new();
    let dropped = vec![VocabularySnapshot {
        version: 1,
        labels: vec!["error-span".to_string()],
    }];
    assert!(registry.restore_vocabulary_snapshots(dropped).is_err());
}

/// A version GAP restores: `update_vocabulary` never requires versions to be
/// consecutive (version 1 then 3 is a legal additions-only session), so the
/// restore must accept exactly that sequence instead of rejecting a reload the
/// running session could have produced. A restore stricter than the session
/// that produced it would silently drop vocabulary history on restart.
#[test]
fn vocabulary_restore_accepts_a_version_gap_update_vocabulary_permits() {
    let mut registry = FilterRegistry::new();
    let mut three = Vocabulary::v1();
    three.version = 3;
    three.labels = vec!["error-span", "identifier", "commit-hash"];
    // The exact in-session sequence: v1, then a jump to v3 (no v2 exists).
    assert_eq!(registry.update_vocabulary(three.clone()).unwrap(), 3);
    let snapshots = registry.vocabulary_snapshots();
    assert_eq!(
        snapshots.iter().map(|s| s.version).collect::<Vec<_>>(),
        vec![1, 3],
        "update_vocabulary records a gap: version 2 never exists"
    );

    let mut restarted = FilterRegistry::new();
    restarted
        .restore_vocabulary_snapshots(snapshots.clone())
        .expect("a prefix-consistent gap restores: the session could produce it");
    assert_eq!(
        restarted.vocabulary().version,
        3,
        "the restored current version is 3, not 2"
    );
    let restored = restarted.vocabulary_snapshots();
    assert_eq!(
        restored.iter().map(|s| s.version).collect::<Vec<_>>(),
        vec![1, 3],
        "restore is identity over the gapped history"
    );
    assert_eq!(
        restored
            .iter()
            .map(|s| s.labels.clone())
            .collect::<Vec<_>>(),
        snapshots
            .iter()
            .map(|s| s.labels.clone())
            .collect::<Vec<_>>(),
        "each version resolves under its own labels after the gap restore"
    );
}

#[test]
fn vocabulary_restore_refuses_an_empty_history() {
    let mut registry = FilterRegistry::new();
    assert!(
        registry.restore_vocabulary_snapshots(Vec::new()).is_err(),
        "an empty vocabulary history must be refused at restore time"
    );
    // The refusal adopted nothing: the registry still resolves its seeded v1.
    assert_eq!(registry.vocabulary().version, 1);
    let refused = registry
        .restore_vocabulary_snapshots(Vec::new())
        .unwrap_err();
    assert_eq!(
        refused.name(),
        "tightening-requires-offline",
        "the refusal is the registry's typed rejected mode"
    );
    assert_eq!(registry.vocabulary_history().len(), 1);
}

/// The FIRST persisted entry claims version 1 — the code's own baseline rules,
/// not just the ordinal. A history whose baseline entry carries tighter or
/// looser rules would silently redefine what every digest resolved at version 1
/// means, so restore refuses it with a typed error (issue #141 regression).
#[test]
fn restore_refuses_history_with_deviant_baseline_rules() {
    let mut registry = FilterRegistry::new();

    // Tighter than the code's baseline: a smaller size floor at version 1.
    let mut tighter = FilterRules::v1();
    tighter.size_floor = 512;
    let refused = registry.restore_histories(vec![tighter]).unwrap_err();
    assert_eq!(
        refused.name(),
        "baseline-rules-mismatch",
        "the refusal is the typed baseline mismatch, not offline tightening"
    );

    // Looser than the code's baseline: a verbatim tool the code's version 1
    // never routed verbatim.
    let mut looser = FilterRules::v1();
    looser.verbatim_tools = vec!["git".to_string()];
    assert!(
        registry.restore_histories(vec![looser]).is_err(),
        "a looser-than-baseline version 1 is refused too"
    );

    // Nothing was adopted: the registry still resolves its seeded v1.
    assert_eq!(registry.rules_history().len(), 1);
    assert_eq!(registry.rules().size_floor, FilterRules::v1().size_floor);
    assert_eq!(
        registry.rules().verbatim_tools,
        FilterRules::v1().verbatim_tools
    );
}

/// A history that is exactly what a legal session produces — the code's own
/// baseline v1 entry followed by strictly newer relaxations — restores
/// unchanged, with every recorded version resolving under its own rules.
#[test]
fn restore_accepts_legitimate_rule_sequences() {
    let mut registry = FilterRegistry::new();
    let mut v2 = FilterRules::v1();
    v2.version = 2;
    v2.size_floor = 2048;
    v2.unknown_bound = 128;
    v2.verbatim_tools = vec!["git".to_string()];
    assert_eq!(registry.update_rules(v2.clone()).unwrap(), 2);

    let history = registry.rules_history().to_vec();
    let mut restarted = FilterRegistry::new();
    restarted
        .restore_histories(history.clone())
        .expect("the code's baseline entry followed by relaxations restores");
    assert_eq!(restarted.rules().version, 2);
    assert_eq!(
        restarted.rules_at(1).map(|rules| rules.size_floor),
        Some(FilterRules::v1().size_floor),
        "version 1 still resolves under the code's baseline rules"
    );
    assert_eq!(
        restarted.rules_at(2).map(|rules| rules.size_floor),
        Some(2048),
        "version 2 resolves under its own relaxed rules"
    );
    assert_eq!(
        restarted.rules_history().len(),
        history.len(),
        "the restore is a round trip over the rule history"
    );
}
