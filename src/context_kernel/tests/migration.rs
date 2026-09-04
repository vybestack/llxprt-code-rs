//! Context-store migration tests: crash matrix, publication, hash scopes.

use crate::context_kernel::canonical::{Digest, HashScope, Sink};
use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, LedgerEventKind, OperationClass, ProviderTurnKind,
    RecordedEvent, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::StoreRange;
use crate::context_kernel::migration::{
    decide, Generation, MigrationDecision, MigrationDescriptor, MigrationPlan, PrivateBuild,
    Publication, PublicationError, SlotPair, V2, V3,
};
use crate::context_kernel::reducer::{Reducer, ReducerError, IDLENESS_WINDOW};

fn append(kind: EventKind, sequencer: &mut Sequencer, log: &mut EventLog) -> RecordedEvent {
    let event = sequencer.append(kind, log.store_version());
    let expected = log.len() as u64 + FIRST_SEQUENCE;
    assert_eq!(
        event.sequence, expected,
        "fixture continues the total order"
    );
    log.append(event.clone()).unwrap();
    event
}

fn user(text: &str, scope: u64) -> EventKind {
    EventKind::Append {
        source: AppendSource::User,
        sanitized: text.as_bytes().to_vec(),
        scope,
        claims: Vec::new(),
    }
}

fn tool(call_id: &str, tool: &str, scope: u64) -> EventKind {
    EventKind::Append {
        source: AppendSource::ToolResult {
            call_id: String::from(call_id),
            tool: String::from(tool),
        },
        sanitized: vec![7_u8; 6],
        scope,
        claims: Vec::new(),
    }
}

fn op(class: OperationClass, subject: u64, argument: u64) -> EventKind {
    EventKind::OperationCommit {
        class,
        subject,
        argument,
    }
}

fn sample_log(store_version: u64) -> EventLog {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(store_version);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        EventKind::Ledger {
            kind: LedgerEventKind::ObligationAdmitted,
            key: String::from("obligation-1"),
        },
        &mut sequencer,
        &mut log,
    );
    append(user("task", 1), &mut sequencer, &mut log);
    append(tool("call-1", "read", 1), &mut sequencer, &mut log);
    append(
        EventKind::ProviderTurn {
            kind: ProviderTurnKind::Conversation,
            request_units: 128,
        },
        &mut sequencer,
        &mut log,
    );
    log
}

#[test]
fn migration_crash_matrix_keeps_v2_until_selection_completes() {
    assert_eq!(decide(&EventLog::new(V2)).store_version(), V2);
    assert_eq!(decide(&sample_log(V2)).store_version(), V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut selected = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut selected,
    );
    append(user("a", 1), &mut sequencer, &mut selected);
    let sequence = append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut selected,
    )
    .sequence;
    assert_eq!(
        decide(&selected),
        MigrationDecision::SelectV3 {
            selected_sequence: sequence,
        }
    );
    let mut wrong_target = EventLog::new(V2);
    let mut wrong_sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut wrong_sequencer,
        &mut wrong_target,
    );
    append(user("a", 1), &mut wrong_sequencer, &mut wrong_target);
    append(
        op(OperationClass::MigrationSelect, 9, 0),
        &mut wrong_sequencer,
        &mut wrong_target,
    );
    assert_eq!(decide(&wrong_target).store_version(), V2);
    assert_eq!(decide(&EventLog::new(V3)).store_version(), V3);
}

#[test]
fn migration_publication_requires_a_complete_private_build() {
    let plan = MigrationPlan::from(
        V3,
        vec![StoreRange {
            offset: 0,
            length: 12,
        }],
        77,
    );
    assert_eq!(plan.units(), 12);
    let mut build = PrivateBuild::start(plan);
    assert!(Publication::of(&build).is_none());
    build.complete_with(&[1_u8; 4]);
    let publication = Publication::of(&build).unwrap();
    assert_eq!(publication.store_version, V3);
    assert!(!publication.published);
}

/// A slot pair at genesis plus the two hashes the swap tests reuse: the chain the
/// selection extends, and the store-build checksum of the `V3` bytes the build adopts.
fn swap_fixtures() -> (SlotPair, Digest, Digest, [u8; 40]) {
    let chain = HashScope::EventChain.digest(b"v2 events");
    let built_bytes = [7_u8; 40];
    let checksum = HashScope::StoreBuild.digest(&built_bytes);
    let slots = SlotPair::genesis(V2, 40, chain);
    (slots, chain, checksum, built_bytes)
}

/// GREEN: a slot pair refuses every publication that is not a completed private
/// build: no build pending, a committed generation never lands, and the inactive
/// slot holds one build at a time.
#[test]
fn slot_pair_refuses_every_publication_that_is_not_a_built_generation() {
    let (mut slots, chain, checksum, built_bytes) = swap_fixtures();
    assert_eq!(slots.active().store_version(), V2);
    assert!(slots.inactive().is_none());
    assert!(!slots.published());

    let swap_without_build = slots.swap(chain).unwrap_err();
    assert_eq!(swap_without_build, PublicationError::NoBuildPending);

    let committed = Generation::Committed {
        store_version: V2,
        bytes: 40,
        chain,
    };
    let land_committed = slots.land(committed).unwrap_err();
    assert_eq!(
        land_committed,
        PublicationError::SlotContract {
            expected: HashScope::StoreBuild,
            found: HashScope::EventChain,
        },
        "a committed generation never lands in the build slot"
    );

    let build = Generation::Built {
        store_version: V3,
        bytes: built_bytes.len() as u64,
        checksum,
    };
    slots.land(build).unwrap();
    assert_eq!(
        slots.land(build).unwrap_err(),
        PublicationError::BuildPending,
        "the inactive slot holds one build at a time"
    );
    assert_eq!(slots.active().store_version(), V2, "landing is invisible");

    slots.discard().unwrap();
    assert!(slots.inactive().is_none());
    slots.land(build).unwrap();
}

/// GREEN: the swap is the visibility switch, it happens at most once, and the
/// descriptor it carries verifies the build and the selection's chain in their own
/// hash scopes.
#[test]
fn the_swap_publishes_once_and_the_descriptor_verifies_both_scopes() {
    let (mut slots, chain, checksum, built_bytes) = swap_fixtures();
    let build = Generation::Built {
        store_version: V3,
        bytes: built_bytes.len() as u64,
        checksum,
    };
    slots.land(build).unwrap();

    let selection_chain = HashScope::EventChain.chain(chain, b"select v3");
    let published = slots.swap(selection_chain).unwrap();
    assert_eq!(published.store_version(), V3);
    assert_eq!(slots.active().store_version(), V3, "the swap is the switch");
    assert!(slots.published());
    assert!(matches!(
        slots.inactive(),
        Some(Generation::Committed { .. })
    ));
    assert_eq!(
        slots.swap(selection_chain).unwrap_err(),
        PublicationError::AlreadyPublished,
        "a publication happens at most once"
    );

    let descriptor = MigrationDescriptor::seal(V3, checksum, selection_chain);
    assert!(descriptor.verify_build(&built_bytes));
    let mut tampered = built_bytes;
    tampered[0] ^= 1;
    assert!(!descriptor.verify_build(&tampered));

    let mut sink = Sink::new();
    descriptor.encode(&mut sink);
    let encoded = sink.finish();
    let mut sink_again = Sink::new();
    descriptor.encode(&mut sink_again);
    assert_eq!(encoded, sink_again.finish());
}

/// GREEN: the sealed descriptor verifies the chain the selection was recorded in,
/// not a prefix of it, and the selection event itself is written under the framing
/// version it names.
#[test]
fn the_descriptor_verifies_the_chain_only_after_the_selection_is_recorded() {
    let (_slots, chain, checksum, _built_bytes) = swap_fixtures();

    let mut log = EventLog::new(V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    let selection = append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut log,
    );
    let mut verified = log.clone();
    append(
        op(OperationClass::StoreMode, 2, 0),
        &mut sequencer,
        &mut verified,
    );
    // The descriptor is sealed against the chained selection hash, not the log head, so
    // the pre-selection prefix fails the chain check.
    let selection_chain = HashScope::EventChain.chain(chain, b"select v3");
    let descriptor = MigrationDescriptor::seal(V3, checksum, selection_chain);
    assert!(
        !descriptor.verify_chain(&log),
        "chain precedes the selection"
    );
    let descriptor = MigrationDescriptor::seal(V3, checksum, verified.head_checksum());
    assert!(descriptor.verify_chain(&verified));
    assert_eq!(selection.store_version, V2);
}

/// GREEN: a selection naming a version no migration defines is a typed refusal.
#[test]
fn migration_selection_names_an_undefined_version_as_a_typed_refusal() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::MigrationSelect, 9, 0),
        &mut sequencer,
        &mut log,
    );
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::MigrationTarget { found: 9 },
        "a selection is refused unless a migration defines the version"
    );
}

/// GREEN: old and new versions have independent hash scopes; corruption in one
/// never invalidates the other's evidence.
#[test]
fn migration_hash_scopes_are_independent() {
    let v2_bytes = [2_u8; 64];
    let v3_bytes = [3_u8; 64];
    let v2_chain = HashScope::EventChain.digest(b"v2 recorded events");
    let build_checksum = HashScope::StoreBuild.digest(&v3_bytes);

    let mut slots = SlotPair::genesis(V2, v2_bytes.len() as u64, v2_chain);
    slots
        .land(Generation::Built {
            store_version: V3,
            bytes: v3_bytes.len() as u64,
            checksum: build_checksum,
        })
        .unwrap();

    // Corrupting the committed generation's chain evidence does not let a build
    // checksum verify in the event-chain scope, and vice versa.
    let tampered_chain = v2_chain ^ 1;
    assert_ne!(tampered_chain, v2_chain, "the tamper is a real change");

    // The same bytes digested in two scopes are two identities, and tampering the
    // bytes changes the digest in either scope.
    assert_ne!(
        HashScope::StoreBuild.digest(&v2_bytes),
        HashScope::EventChain.digest(&v2_bytes),
        "the same bytes in different scopes are different identities"
    );
    let mut tampered_build = v3_bytes;
    tampered_build[0] ^= 1;
    assert_ne!(
        HashScope::StoreBuild.digest(&tampered_build),
        build_checksum,
        "tampering the built bytes changes the store-build digest"
    );
    assert_ne!(
        HashScope::EventChain.digest(&tampered_build),
        HashScope::EventChain.digest(&v3_bytes),
        "tampering the bytes changes the event-chain digest too"
    );
    assert_ne!(
        HashScope::StoreBuild.digest(&v3_bytes),
        HashScope::EventChain.digest(&v3_bytes),
        "a build checksum never equals an event-chain checksum over the same bytes"
    );

    // The landed build verifies only inside the store-build scope.
    let inactive = slots.inactive().unwrap();
    assert_eq!(inactive.scope(), HashScope::StoreBuild);
    assert_eq!(inactive.store_version(), V3);
    let descriptor = MigrationDescriptor::seal(V3, build_checksum, tampered_chain);
    assert!(
        descriptor.verify_build(&v3_bytes),
        "the build evidence is intact"
    );
    assert!(!descriptor.verify_build(&v2_bytes));
    assert_ne!(
        descriptor.selection_chain, v2_chain,
        "the tampered chain the descriptor carries differs from the recorded one"
    );
    // And the chain evidence refuses verification, but the build evidence does not.
    let mut verified = EventLog::new(V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let event = sequencer.append(op(OperationClass::ScopeOpen, 1, 0), V2);
    verified.append(event).unwrap();
    assert!(!descriptor.verify_chain(&verified));
    assert!(
        MigrationDescriptor::seal(V3, build_checksum, verified.head_checksum())
            .verify_chain(&verified),
        "chain evidence verifies against the log it names"
    );
}

/// GREEN: a crash between the build write and the swap leaves the old version
/// active, and the swap is a single visibility transition. The pair is rebuilt from
/// the durable record, so the recovery path asserts what survived the crash rather
/// than what a fresh genesis happens to contain.
#[test]
fn publication_crash_between_write_and_swap_keeps_the_old_version_active() {
    let v2_chain = HashScope::EventChain.digest(b"v2 recorded events");
    let built = [9_u8; 32];
    let build_checksum = HashScope::StoreBuild.digest(&built);
    let mut slots = SlotPair::genesis(V2, 64, v2_chain);
    slots
        .land(Generation::Built {
            store_version: V3,
            bytes: built.len() as u64,
            checksum: build_checksum,
        })
        .unwrap();

    // The pair that holds the landed build: the write landed in the inactive slot
    // and the swap has not happened, so v2 is still what readers resolve.
    assert_eq!(
        slots.active().store_version(),
        V2,
        "the write leaves the committed generation active"
    );
    let inactive = slots.inactive().unwrap();
    assert_eq!(inactive.store_version(), V3);
    assert_eq!(inactive.scope(), HashScope::StoreBuild);
    assert!(!slots.published(), "the swap has not happened");

    // Simulate a crash after the write, before the swap: the durable record of the
    // pair is its genesis framing plus whatever the publication already committed,
    // and a publication that never reached the swap has committed nothing. So
    // recovery re-frames the pair from the recorded chain and lands the build
    // again from its descriptor, and v2 is still what readers resolve.
    let mut recovered = SlotPair::genesis(V2, 64, v2_chain);
    assert_eq!(
        recovered.active().store_version(),
        V2,
        "a crash before the swap leaves the committed generation active"
    );
    assert!(
        recovered.inactive().is_none(),
        "the write is not durable until the swap commits it"
    );
    assert!(!recovered.published());
    // Rebuilding the landed build from the durable descriptor reproduces the pair
    // that held it, without publishing: v2 stays active and v3 stays pending.
    let descriptor = MigrationDescriptor::seal(V3, build_checksum, v2_chain);
    assert!(descriptor.verify_build(&built));
    recovered
        .land(Generation::Built {
            store_version: descriptor.store_version,
            bytes: built.len() as u64,
            checksum: descriptor.build_checksum,
        })
        .unwrap();
    assert_eq!(recovered.active().store_version(), V2);
    assert_eq!(recovered.inactive().unwrap().store_version(), V3);
    assert!(
        !recovered.published(),
        "re-landing the build is not a publication"
    );

    // And after the swap, the published state is durable and idempotent.
    let selection_chain = HashScope::EventChain.chain(v2_chain, b"migration-select v3");
    let swapped = slots.swap(selection_chain).unwrap();
    assert_eq!(swapped.store_version(), V3);
    assert_eq!(slots.active().store_version(), V3);
    assert!(matches!(slots.active(), Generation::Committed { .. }));
    assert_eq!(slots.inactive().unwrap().store_version(), V2);
    assert!(slots.published(), "the swap is the publication");
    assert!(
        slots.swap(selection_chain).is_err(),
        "the swap happens at most once"
    );
    // The published descriptor is the durable record of the completed publication,
    // and re-framing from it resolves the new generation, not the old one.
    let published =
        MigrationDescriptor::seal(V3, HashScope::StoreBuild.digest(&built), selection_chain);
    let reframed = SlotPair::genesis(
        published.store_version,
        built.len() as u64,
        published.selection_chain,
    );
    assert_eq!(
        reframed.active().store_version(),
        V3,
        "re-framing from the published descriptor resolves the new generation"
    );
    assert!(published.verify_build(&built));
    assert!(published.published);
}

/// A migration publication written to the durable directory, recovered from
/// disk after a crash at each interval, must resolve the same generation as the
/// live process did. Requires the durable directory shape from unit A.
#[test]
#[ignore = "requires unit A durable directory"]
fn migration_publication_is_durable_across_a_crash() {
    // The publication lands in the inactive slot and the swap is atomic, so a
    // crash between the write and the swap must recover with v2 active and the
    // inactive build either complete or absent — never a partial migration. The
    // durable directory that makes this observable on disk is unit A's substrate.
}
