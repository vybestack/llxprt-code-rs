//! Table-driven legality gate over typed state and a render contract.
//!
//! Every check is a row in the `RULES` table so the gate is exhaustive by
//! construction and new predicates are added as new rows, never as scattered
//! conditionals. A check returns the violated predicate as text so a rejected
//! send is explainable.

use crate::context_kernel::canonical::Sink;
use crate::context_kernel::ir::Region;
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::reducer::TypedState;

/// Quoting convention used for verbatim lanes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuotingConvention {
    /// Fenced blocks.
    Fenced,
    /// Explicit XML delimiters.
    XmlDelimited,
}

impl QuotingConvention {
    /// Stable name used in canonical encodings and violation text.
    pub fn name(self) -> &'static str {
        match self {
            QuotingConvention::Fenced => "fenced",
            QuotingConvention::XmlDelimited => "xml-delimited",
        }
    }

    /// Encodes the convention into `sink`.
    pub fn encode(self, sink: &mut Sink) {
        sink.tag(self.name());
    }
}

/// Role a tool declaration plays in call/result pairing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolRole {
    /// A tool call awaiting its result.
    Call,
    /// The result answering a call.
    Result,
}

/// One declared tool interaction, used to check pairing integrity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ToolDeclaration {
    /// Tool identity declared by the harness.
    pub tool: String,
    /// Call identity pairing the call with its result.
    pub call_id: String,
    /// Role of this declaration.
    pub role: ToolRole,
}

/// Capability descriptor and budgets the render must fit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RenderContract {
    /// Contract version; the committed version and the sent version must be equal.
    pub version: u64,
    /// Output token ceiling of the target.
    ///
    /// Issue 105-4: carried, not evaluated. `max_output_tokens` prices what
    /// the provider renders FROM the context; admission legality gates what
    /// goes INTO it (region occupancy, floors, pairing), and no field of this
    /// contract bounds the render. The generate call carries its own
    /// `max_tokens` (adapter/profile), so clamping here would silently
    /// truncate governed content instead of surfacing the pressure. The
    /// whole-request ceiling that DOES gate this projection is
    /// `profile_budget_units`, enforced by the `profile-over-budget` rule.
    pub max_output_tokens: u64,
    /// Whether the target can render the notes region at all.
    pub supports_notes_region: bool,
    /// Whether the target can render placeholders.
    pub supports_placeholders: bool,
    /// Quoting convention the target applies to verbatim lanes.
    pub quoting_convention: QuotingConvention,
    /// Profile budget for the whole request, in accounting units.
    ///
    /// Issue 105-4: enforced by the `profile-over-budget` rule, which sums
    /// every region occupancy the projection carries and refuses one that
    /// bursts this whole-request ceiling.
    pub profile_budget_units: u64,
    /// Per-region occupancy budgets, in accounting units.
    pub region_budgets: Vec<(Region, u64)>,
    /// Tool declarations the projection must pair correctly.
    pub tool_declarations: Vec<ToolDeclaration>,
}

impl RenderContract {
    /// A contract with every capability and generous budgets.
    pub fn generous(version: u64) -> Self {
        Self {
            version,
            max_output_tokens: 8192,
            supports_notes_region: true,
            supports_placeholders: true,
            quoting_convention: QuotingConvention::Fenced,
            profile_budget_units: 1_000_000,
            region_budgets: Region::all().map(|region| (region, 1_000_000)).to_vec(),
            tool_declarations: Vec::new(),
        }
    }

    /// Adds a tool declaration.
    pub fn declare(&mut self, tool: &str, call_id: &str, role: ToolRole) {
        self.tool_declarations.push(ToolDeclaration {
            tool: String::from(tool),
            call_id: String::from(call_id),
            role,
        });
    }

    /// Whether the target admits `region` at all.
    pub fn supports_region(&self, region: Region) -> bool {
        region != Region::Notes || self.supports_notes_region
    }

    /// Occupancy budget for `region`; an unsupported region has a budget of zero.
    pub fn region_budget(&self, region: Region) -> u64 {
        if !self.supports_region(region) {
            return 0;
        }
        self.region_budgets
            .iter()
            .find(|(candidate, _)| *candidate == region)
            .map(|(_, budget)| *budget)
            .unwrap_or(u64::MAX)
    }
}

/// One rejected predicate. Each variant carries the text of the predicate that
/// failed, so a rejected send is explainable without re-running the gate.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Violation {
    /// A tool call or result is unpaired.
    Pairing {
        /// Violated predicate.
        predicate: String,
    },
    /// Region placement order is not monotonic.
    Ordering {
        /// Violated predicate.
        predicate: String,
    },
    /// A placeholder was rendered without support for one.
    PlaceholderIllegal {
        /// Violated predicate.
        predicate: String,
    },
    /// A region holds more than its budget.
    RegionOverBudget {
        /// Violated predicate.
        predicate: String,
    },
    /// A lane was reclaimed below its floor.
    Floor {
        /// Violated predicate.
        predicate: String,
    },
    /// A pinned item was dropped or unplaced.
    Pin {
        /// Violated predicate.
        predicate: String,
    },
    /// The quoting convention differs from the contract.
    QuotingConvention {
        /// Violated predicate.
        predicate: String,
    },
    /// The projection exceeds the contract's profile budget.
    ProfileOverBudget {
        /// Violated predicate.
        predicate: String,
    },
}

impl Violation {
    /// Stable name of the violated predicate class.
    pub fn kind(&self) -> &'static str {
        match self {
            Violation::Pairing { .. } => "pairing",
            Violation::Ordering { .. } => "ordering",
            Violation::PlaceholderIllegal { .. } => "placeholder-illegal",
            Violation::RegionOverBudget { .. } => "region-over-budget",
            Violation::Floor { .. } => "floor",
            Violation::Pin { .. } => "pin",
            Violation::QuotingConvention { .. } => "quoting-convention",
            Violation::ProfileOverBudget { .. } => "profile-over-budget",
        }
    }

    /// Text of the violated predicate.
    pub fn predicate(&self) -> &str {
        match self {
            Violation::Pairing { predicate }
            | Violation::Ordering { predicate }
            | Violation::PlaceholderIllegal { predicate }
            | Violation::RegionOverBudget { predicate }
            | Violation::Floor { predicate }
            | Violation::Pin { predicate }
            | Violation::QuotingConvention { predicate }
            | Violation::ProfileOverBudget { predicate } => predicate,
        }
    }
}

/// Proof that a projection is legal under one contract version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LegalContext {
    contract_version: u64,
}

impl LegalContext {
    /// Contract version the projection was committed under.
    pub fn contract_version(&self) -> u64 {
        self.contract_version
    }

    /// Whether a send carrying `send_version` matches the committed contract.
    pub fn sendable_with(&self, send_version: u64) -> bool {
        self.contract_version == send_version
    }
}

/// Signature shared by every row of the legality table.
pub type Check = fn(&TypedState, &RenderContract) -> Option<Violation>;

/// One row of the legality table.
pub struct Rule {
    /// Stable predicate name.
    pub name: &'static str,
    /// Predicate implementation.
    pub check: Check,
}

/// The legality table, in precedence order.
const RULES: &[Rule] = &[
    Rule {
        name: "pairing",
        check: check_pairing,
    },
    Rule {
        name: "ordering",
        check: check_ordering,
    },
    Rule {
        name: "placeholder-illegal",
        check: check_placeholder,
    },
    Rule {
        name: "region-over-budget",
        check: check_region_budget,
    },
    Rule {
        name: "profile-over-budget",
        check: check_profile_budget,
    },
    Rule {
        name: "floor",
        check: check_floor,
    },
    Rule {
        name: "pin",
        check: check_pin,
    },
    Rule {
        name: "quoting-convention",
        check: check_quoting,
    },
];

/// The legality table, in precedence order.
pub fn rules() -> &'static [Rule] {
    RULES
}

/// Runs every row of the table, returning the first violated predicate.
pub fn is_legal(state: &TypedState, contract: &RenderContract) -> Result<LegalContext, Violation> {
    for rule in RULES {
        if let Some(violation) = (rule.check)(state, contract) {
            return Err(violation);
        }
    }
    Ok(LegalContext {
        contract_version: contract.version,
    })
}

fn count_role(contract: &RenderContract, call_id: &str, role: ToolRole) -> usize {
    contract
        .tool_declarations
        .iter()
        .filter(|declaration| declaration.call_id == call_id && declaration.role == role)
        .count()
}

fn check_pairing(_state: &TypedState, contract: &RenderContract) -> Option<Violation> {
    for declaration in &contract.tool_declarations {
        let calls = count_role(contract, &declaration.call_id, ToolRole::Call);
        let results = count_role(contract, &declaration.call_id, ToolRole::Result);
        if calls != 1 || results != 1 {
            let predicate = format!(
                "call-result pairing for {} is {calls} call(s) and {results} result(s)",
                declaration.call_id
            );
            return Some(Violation::Pairing { predicate });
        }
    }
    None
}

fn check_ordering(state: &TypedState, _contract: &RenderContract) -> Option<Violation> {
    let mut highest: Option<Region> = None;
    for item in state.conversation_ir.items() {
        let Some(region) = item.region() else {
            continue;
        };
        let Some(previous) = highest else {
            highest = Some(region);
            continue;
        };
        if region < previous {
            let predicate = format!(
                "item {} placed in {} after {}",
                item.id().value(),
                region.name(),
                previous.name()
            );
            return Some(Violation::Ordering { predicate });
        }
        highest = Some(region);
    }
    None
}

fn check_placeholder(state: &TypedState, contract: &RenderContract) -> Option<Violation> {
    if contract.supports_placeholders {
        return None;
    }
    for item in state.conversation_ir.items() {
        if item.is_placeholder() {
            let predicate = format!(
                "item {} is a placeholder but the contract admits none",
                item.id().value()
            );
            return Some(Violation::PlaceholderIllegal { predicate });
        }
    }
    None
}

/// Issue 105-4: `profile_budget_units` is the whole-request ceiling, so the
/// gate sums every region occupancy the projection carries and refuses a
/// projection that exceeds it. Without this row the field is carried and
/// never read - exactly the dead surface the reviewer flagged - and a
/// projection could be legal while bursting past the budget the profile
/// declares for the whole request.
fn check_profile_budget(state: &TypedState, contract: &RenderContract) -> Option<Violation> {
    let mut total = 0u64;
    for region in Region::all() {
        total = total.saturating_add(state.conversation_ir.region_occupancy(region));
    }
    if total > contract.profile_budget_units {
        let predicate = format!(
            "projection holds {total} unit(s) across all regions against a profile budget of {} unit(s)",
            contract.profile_budget_units
        );
        return Some(Violation::ProfileOverBudget { predicate });
    }
    None
}

fn check_region_budget(state: &TypedState, contract: &RenderContract) -> Option<Violation> {
    for region in Region::all() {
        let occupancy = state.conversation_ir.region_occupancy(region);
        let budget = contract.region_budget(region);
        if occupancy > budget {
            let predicate = format!(
                "region {} holds {occupancy} unit(s) against a budget of {budget} unit(s)",
                region.name()
            );
            return Some(Violation::RegionOverBudget { predicate });
        }
    }
    None
}

fn check_floor(state: &TypedState, _contract: &RenderContract) -> Option<Violation> {
    for lane in Lane::all() {
        let Ok(policy) = state.lane_policy_registry.policy(lane) else {
            continue;
        };
        let total = state.conversation_ir.lane_units(lane);
        let placed = state.conversation_ir.lane_placed_units(lane);
        if total > placed && placed < policy.floor_units {
            let predicate = format!(
                "lane {} retains {placed} of {total} unit(s), below its floor of {} unit(s)",
                lane.name(),
                policy.floor_units
            );
            return Some(Violation::Floor { predicate });
        }
    }
    None
}

fn check_pin(state: &TypedState, _contract: &RenderContract) -> Option<Violation> {
    for pin in &state.pins {
        let Ok(item) = state.conversation_ir.item(*pin) else {
            let predicate = format!(
                "pinned item {} is absent from the conversation ir",
                pin.value()
            );
            return Some(Violation::Pin { predicate });
        };
        if item.region().is_none() {
            let predicate = format!("pinned item {} is unplaced", pin.value());
            return Some(Violation::Pin { predicate });
        }
    }
    None
}

fn check_quoting(state: &TypedState, contract: &RenderContract) -> Option<Violation> {
    if state.quoting_convention == contract.quoting_convention {
        return None;
    }
    let predicate = format!(
        "state quotes {} but the contract declares {}",
        state.quoting_convention.name(),
        contract.quoting_convention.name()
    );
    Some(Violation::QuotingConvention { predicate })
}
