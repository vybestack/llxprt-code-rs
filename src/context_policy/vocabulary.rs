//! Typed vocabulary for the durable policy-event names and the terminal
//! branch labels.
//!
//! Two spellings per concept are load-bearing for durable compatibility
//! (issue 122): policy events carry HYPHENATED operation names (`events.log`),
//! while the manifest's `terminal_outcome` carries UNDERSCORED branch labels.
//! The bytes must never be unified, so each enum is the single place its
//! spelling is written down and parsed back.

/// One durable policy-event operation: the name an `events.log` line carries
/// in its `operation` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PolicyOperation {
    /// The quota refused an admission (`Admission::Quiesce` completion path).
    QuiesceRate,
    /// A store write failed (`abort_bulk` and the write-failure wrap-up).
    QuiesceUnwritable,
    /// The reclamation recorded when no ladder rung names the operation.
    DropWithHandle,
    /// The explicit session-finalization event.
    WrapUp,
    /// Ladder rung: fold away ephemeral records.
    FoldAwayEphemeral,
    /// Ladder rung: collapse placeholders into their digests.
    PlaceholderCollapse,
    /// Ladder rung: fold records together.
    Fold,
    /// Ladder rung: compact the region.
    Compact,
    /// Ladder rung: condense the region.
    Condense,
}

impl PolicyOperation {
    /// Every durable operation name, the fixed escalation rungs last.
    pub const fn all() -> [PolicyOperation; 9] {
        [
            Self::QuiesceRate,
            Self::QuiesceUnwritable,
            Self::DropWithHandle,
            Self::WrapUp,
            Self::FoldAwayEphemeral,
            Self::PlaceholderCollapse,
            Self::Fold,
            Self::Compact,
            Self::Condense,
        ]
    }

    /// The hyphenated spelling this operation carries durably in
    /// `events.log` — the only place these bytes are spelled.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QuiesceRate => "quiesce-rate",
            Self::QuiesceUnwritable => "quiesce-unwritable",
            Self::DropWithHandle => "drop-with-handle",
            Self::WrapUp => "wrap-up",
            Self::FoldAwayEphemeral => "fold-away-ephemeral",
            Self::PlaceholderCollapse => "placeholder-collapse",
            Self::Fold => "fold",
            Self::Compact => "compact",
            Self::Condense => "condense",
        }
    }

    /// Parses a reloaded `events.log` operation name, rejecting unknown
    /// names with the exact message the durable reader has always produced.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "quiesce-rate" => Ok(Self::QuiesceRate),
            "quiesce-unwritable" => Ok(Self::QuiesceUnwritable),
            "drop-with-handle" => Ok(Self::DropWithHandle),
            "wrap-up" => Ok(Self::WrapUp),
            "fold-away-ephemeral" => Ok(Self::FoldAwayEphemeral),
            "placeholder-collapse" => Ok(Self::PlaceholderCollapse),
            "fold" => Ok(Self::Fold),
            "compact" => Ok(Self::Compact),
            "condense" => Ok(Self::Condense),
            other => Err(format!("context policy event unknown operation: {other}")),
        }
    }
}

impl std::str::FromStr for PolicyOperation {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name)
    }
}

/// One terminal branch label: the spelling the manifest's `terminal_outcome`
/// field and every recovery test pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerminalLabel {
    /// The quota's own refusal (`quiesce_rate`).
    QuiesceRate,
    /// A store write failure (`quiesce_unwritable`).
    QuiesceUnwritable,
    /// A feasible session finalization (`wrap_up`).
    WrapUp,
}

impl TerminalLabel {
    /// The underscored spelling this label carries durably in the manifest —
    /// the only place these bytes are spelled. They are deliberately NOT the
    /// hyphenated `PolicyOperation` spellings; do not unify them.
    pub const fn as_terminal_str(self) -> &'static str {
        match self {
            Self::QuiesceRate => "quiesce_rate",
            Self::QuiesceUnwritable => "quiesce_unwritable",
            Self::WrapUp => "wrap_up",
        }
    }

    /// Parses a restored manifest label, or `None` for a name no terminal
    /// ever wrote.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "quiesce_rate" => Some(Self::QuiesceRate),
            "quiesce_unwritable" => Some(Self::QuiesceUnwritable),
            "wrap_up" => Some(Self::WrapUp),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PolicyOperation, TerminalLabel};
    use crate::context_policy::runtime::PolicyEvent;

    /// The durable event spellings today's builds write into `events.log`,
    /// copied verbatim so a drift fails here instead of at recovery time.
    const EVENT_SPELLINGS: [(&str, PolicyOperation); 9] = [
        ("quiesce-rate", PolicyOperation::QuiesceRate),
        ("quiesce-unwritable", PolicyOperation::QuiesceUnwritable),
        ("drop-with-handle", PolicyOperation::DropWithHandle),
        ("wrap-up", PolicyOperation::WrapUp),
        ("fold-away-ephemeral", PolicyOperation::FoldAwayEphemeral),
        ("placeholder-collapse", PolicyOperation::PlaceholderCollapse),
        ("fold", PolicyOperation::Fold),
        ("compact", PolicyOperation::Compact),
        ("condense", PolicyOperation::Condense),
    ];

    /// The durable terminal spellings the manifest's `terminal_outcome`
    /// carries, copied verbatim from today's pinned behavior.
    const TERMINAL_SPELLINGS: [(&str, TerminalLabel); 3] = [
        ("quiesce_rate", TerminalLabel::QuiesceRate),
        ("quiesce_unwritable", TerminalLabel::QuiesceUnwritable),
        ("wrap_up", TerminalLabel::WrapUp),
    ];

    #[test]
    fn every_operation_round_trips_its_durable_spelling() {
        for (spelling, operation) in EVENT_SPELLINGS {
            assert_eq!(PolicyOperation::parse(spelling), Ok(operation));
            assert_eq!(operation.as_str(), spelling);
            assert_eq!(spelling.parse::<PolicyOperation>(), Ok(operation));
        }
    }

    #[test]
    fn the_durable_event_reader_keeps_every_operation_byte_identical() {
        for (spelling, _) in EVENT_SPELLINGS {
            let line = format!(
                "{{\"logical_time\":1,\"source\":2,\"operation\":\"{spelling}\",\"input_bytes\":3,\"admitted_bytes\":4,\"reclaimed_bytes\":5,\"armed_before\":false,\"armed_after\":true}}"
            );
            let value: serde_json::Value =
                serde_json::from_str(&line).expect("the pinned line parses");
            let event = PolicyEvent::from_json(&value).expect("the durable spelling parses");
            assert_eq!(event.operation, spelling);
            let round: String = serde_json::to_string(&event).expect("event serializes");
            assert!(
                round.contains(&format!("\"operation\":\"{spelling}\"")),
                "the round trip must re-emit the identical spelling: {round}"
            );
            let reparsed: serde_json::Value =
                serde_json::from_str(&round).expect("the round trip parses");
            let again = PolicyEvent::from_json(&reparsed).expect("the round trip restores");
            assert_eq!(again.operation, spelling);
        }
    }

    #[test]
    fn unknown_operations_keep_the_reader_error_text() {
        let expected = "context policy event unknown operation: made-up".to_string();
        assert_eq!(PolicyOperation::parse("made-up"), Err(expected.clone()));
        let value: serde_json::Value = serde_json::from_str(
            "{\"logical_time\":1,\"source\":2,\"operation\":\"made-up\",\"input_bytes\":3,\"admitted_bytes\":4,\"reclaimed_bytes\":5,\"armed_before\":false,\"armed_after\":true}",
        )
        .expect("the line parses");
        let error = PolicyEvent::from_json(&value).expect_err("an unknown operation refuses");
        assert_eq!(error, expected);
    }

    #[test]
    fn every_terminal_label_renders_the_pinned_manifest_bytes() {
        for (spelling, label) in TERMINAL_SPELLINGS {
            assert_eq!(label.as_terminal_str(), spelling);
            assert_eq!(TerminalLabel::parse(spelling), Some(label));
        }
        // The two vocabularies stay distinct: no event spelling doubles as a
        // terminal label, and the other way round.
        assert_eq!(TerminalLabel::parse("quiesce-rate"), None);
        assert!(PolicyOperation::parse("wrap_up").is_err());
        assert_ne!(
            TerminalLabel::WrapUp.as_terminal_str(),
            PolicyOperation::WrapUp.as_str()
        );
    }
}
