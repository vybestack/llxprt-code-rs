//! Conversation intermediate representation: claim-atomic items over store bytes.
//!
//! Items partition by lane, not by message: one append can be resegmented into
//! several items whose byte provenance still covers the parent exactly. Placement
//! is exclusive (an item sits in at most one region) and occupancy is charged to
//! the owning region exactly once.

use crate::context_kernel::canonical::Sink;
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::scopes::ScopeId;

/// Render region of the projection layout, in occupancy order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Region {
    /// Protected prefix carrying constitutional items.
    Head,
    /// Durable side channel for commitments and standing decisions.
    Notes,
    /// Working conversation body.
    Body,
    /// Reclamation tail.
    Tail,
}

impl Region {
    /// All regions in occupancy order.
    pub fn all() -> [Region; 4] {
        [Region::Head, Region::Notes, Region::Body, Region::Tail]
    }

    /// Stable rank used in canonical encodings and operation arguments.
    pub fn rank(self) -> u64 {
        match self {
            Region::Head => 1,
            Region::Notes => 2,
            Region::Body => 3,
            Region::Tail => 4,
        }
    }

    /// Stable name used in violation descriptions.
    pub fn name(self) -> &'static str {
        match self {
            Region::Head => "head",
            Region::Notes => "notes",
            Region::Body => "body",
            Region::Tail => "tail",
        }
    }

    /// Resolves a region from its stable rank.
    pub fn from_rank(rank: u64) -> Option<Region> {
        match rank {
            1 => Some(Region::Head),
            2 => Some(Region::Notes),
            3 => Some(Region::Body),
            4 => Some(Region::Tail),
            _ => None,
        }
    }

    /// Encodes the region into `sink`.
    pub fn encode(self, sink: &mut Sink) {
        sink.tag("region");
        sink.int(self.rank());
    }
}

/// Namespace an item identifier is unique within. Append items and resegment
/// children mint identifiers from independent sequences; the namespace keeps a
/// split child from ever colliding with a later append (or vice versa) with no
/// cross-sequence reservation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ItemNamespace {
    /// Identifiers minted for whole appends, from the event sequence.
    Append,
    /// Identifiers minted by resegmentation for claim-atomic children.
    Split,
}

impl ItemNamespace {
    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            ItemNamespace::Append => 1,
            ItemNamespace::Split => 2,
        }
    }

    /// Namespace named by a canonical discriminant.
    pub fn from_code(code: u64) -> Option<ItemNamespace> {
        match code {
            1 => Some(ItemNamespace::Append),
            2 => Some(ItemNamespace::Split),
            _ => None,
        }
    }
}

/// Immutable identifier of an item: a namespace plus a value, unique as a pair.
/// Identifiers are never rewritten and never reused within their namespace:
/// resegmentation mints fresh split identifiers for the children and retires the
/// parent identifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ItemId {
    namespace: ItemNamespace,
    value: u64,
}

impl ItemId {
    /// Wraps an append identifier: the event sequence that produced the append.
    pub fn append(value: u64) -> Self {
        Self {
            namespace: ItemNamespace::Append,
            value,
        }
    }

    /// Wraps a resegmentation identifier.
    pub fn split(value: u64) -> Self {
        Self {
            namespace: ItemNamespace::Split,
            value,
        }
    }

    /// Wraps a raw identifier value in the append namespace, for operation
    /// subjects that name an item by a single integer.
    pub fn new(value: u64) -> Self {
        Self::append(value)
    }

    /// Namespace the identifier is unique within.
    pub fn namespace(self) -> ItemNamespace {
        self.namespace
    }

    /// Identifier value inside its namespace.
    pub fn value(self) -> u64 {
        self.value
    }

    /// Builds an identifier from a recorded namespace discriminant and value,
    /// refusing a discriminant no namespace defines.
    pub fn from_parts(code: u64, value: u64) -> Result<Self, IrError> {
        let namespace =
            ItemNamespace::from_code(code).ok_or(IrError::UnknownNamespace { found: code })?;
        Ok(Self { namespace, value })
    }

    /// Encodes the identifier into `sink`.
    pub fn encode(self, sink: &mut Sink) {
        sink.tag("item");
        sink.int(self.namespace.code());
        sink.int(self.value);
    }
}

/// Structural class of one claim, recorded by segmentation. The names are the
/// documented structural classes; the kernel resolves each to a lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StructuralClass {
    /// Verbatim span the render contract must reproduce exactly.
    ExactSpan,
    /// Identifier line.
    Identifier,
    /// Source code.
    Code,
    /// Test log.
    TestLog,
    /// Noise: foldable, never load-bearing.
    Noise,
    /// Unclassified prose.
    Unknown,
}

impl StructuralClass {
    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            StructuralClass::ExactSpan => 1,
            StructuralClass::Identifier => 2,
            StructuralClass::Code => 3,
            StructuralClass::TestLog => 4,
            StructuralClass::Noise => 5,
            StructuralClass::Unknown => 6,
        }
    }

    /// Class named by a canonical discriminant.
    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(StructuralClass::ExactSpan),
            2 => Some(StructuralClass::Identifier),
            3 => Some(StructuralClass::Code),
            4 => Some(StructuralClass::TestLog),
            5 => Some(StructuralClass::Noise),
            6 => Some(StructuralClass::Unknown),
            _ => None,
        }
    }

    /// Stable name used in canonical encodings.
    pub fn name(self) -> &'static str {
        match self {
            StructuralClass::ExactSpan => "exact-span",
            StructuralClass::Identifier => "identifier",
            StructuralClass::Code => "code",
            StructuralClass::TestLog => "test-log",
            StructuralClass::Noise => "noise",
            StructuralClass::Unknown => "unknown",
        }
    }

    /// Encodes the class into `sink`.
    pub fn encode(self, sink: &mut Sink) {
        sink.tag(self.name());
    }
}

/// One claim-atomic segmentation of an append: a byte span plus the structural
/// class segmentation assigned it. A claim with no class falls back to the
/// structural lane of the append's source, so lane assignment is decided by
/// content wherever segmentation classified any, and by source only as the
/// documented fallback.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SegmentClaim {
    /// Byte span of the claim, relative to the append's own bytes.
    pub span: StoreRange,
    /// Structural class segmentation assigned the span.
    pub class: Option<StructuralClass>,
}

/// A byte range in the context store spine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StoreRange {
    /// Offset of the first byte, from the start of the store spine.
    pub offset: u64,
    /// Length in bytes.
    pub length: u64,
}

impl StoreRange {
    /// Offset one past the last byte.
    pub fn end(self) -> u64 {
        self.offset + self.length
    }
}

/// Merges touching and overlapping ranges into an ordered, disjoint cover.
pub fn normalize(ranges: &[StoreRange]) -> Vec<StoreRange> {
    let mut sorted: Vec<StoreRange> = ranges.iter().copied().filter(|r| r.length > 0).collect();
    sorted.sort_by_key(|range| (range.offset, range.end()));
    let mut merged: Vec<StoreRange> = Vec::new();
    for range in sorted {
        let touching = match merged.last_mut() {
            Some(last) if range.offset <= last.end() => {
                if range.end() > last.end() {
                    last.length = range.end() - last.offset;
                }
                true
            }
            _ => false,
        };
        if !touching {
            merged.push(range);
        }
    }
    merged
}

/// Bytes covered by a set of ranges, counted once.
pub fn covered_units(ranges: &[StoreRange]) -> u64 {
    normalize(ranges).iter().map(|range| range.length).sum()
}

/// Smallest single range enclosing `ranges`.
pub fn enclosing(ranges: &[StoreRange]) -> StoreRange {
    let mut iterator = normalize(ranges).into_iter();
    let first = iterator.next().unwrap_or(StoreRange {
        offset: 0,
        length: 0,
    });
    let mut end = first.end();
    for range in iterator {
        if range.end() > end {
            end = range.end();
        }
    }
    StoreRange {
        offset: first.offset,
        length: end - first.offset,
    }
}

/// Slices an ordered range set into `parts` pieces of near-equal length, walking
/// bytes forward so the pieces are disjoint and together cover the input exactly.
pub fn slice_into(ranges: &[StoreRange], parts: usize) -> Vec<Vec<StoreRange>> {
    let total: u64 = ranges.iter().map(|range| range.length).sum();
    let mut pieces: Vec<Vec<StoreRange>> = Vec::new();
    if parts == 0 {
        return pieces;
    }
    let base = total / parts as u64;
    let extra = total % parts as u64;
    let mut index = 0usize;
    let mut consumed = 0u64;
    for part in 0..parts {
        let mut target = base;
        if (part as u64) < extra {
            target += 1;
        }
        let mut piece: Vec<StoreRange> = Vec::new();
        while target > 0 && index < ranges.len() {
            let current = ranges[index];
            let available = current.length - consumed;
            let take = available.min(target);
            piece.push(StoreRange {
                offset: current.offset + consumed,
                length: take,
            });
            consumed += take;
            target -= take;
            if consumed == current.length {
                index += 1;
                consumed = 0;
            }
        }
        pieces.push(piece);
    }
    pieces
}

/// Explicit placement state of an item. The four states are recorded on the item
/// itself, never inferred from the absence of a placement, so initial items,
/// explicit unplacement, collapsed placeholders, and vaulted redactions are
/// distinguishable in the typed state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// The item is claimed into a region and its units are charged there.
    Placed(Region),
    /// The item keeps its bytes in the store but sits out of every region:
    /// explicit unplacement, vaulted redaction, or a returned store handle.
    StoreOnly,
    /// The item was collapsed to a placeholder and carries no bytes at all.
    Phantom,
}

impl Placement {
    /// Whether the placement claims the item into a region.
    pub fn is_placed(self) -> bool {
        matches!(self, Placement::Placed(_))
    }

    /// Region the placement claims, when placed.
    pub fn region(self) -> Option<Region> {
        match self {
            Placement::Placed(region) => Some(region),
            Placement::StoreOnly | Placement::Phantom => None,
        }
    }

    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            Placement::Placed(_) => 1,
            Placement::StoreOnly => 2,
            Placement::Phantom => 3,
        }
    }
}

/// One claim: a lane, its byte provenance, its scope, and its explicit placement
/// state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    /// Immutable identifier.
    id: ItemId,
    /// Lane the claim belongs to.
    pub lane: Lane,
    /// Byte provenance in the store spine.
    pub provenance: Vec<StoreRange>,
    /// Scope the claim is attributed to.
    pub scope: ScopeId,
    /// Placement state, carried explicitly.
    placement: Placement,
    /// Enclosing byte range of the provenance.
    pub byte_range: StoreRange,
}

/// Whether the placement transition is legal from the item's current state: a
/// phantom is never collapsed again. The guard is placement-only; pins live on
/// [`TypedState`](crate::context_kernel::reducer::TypedState), not on the IR, so
/// collapse itself never consults a pin. Refusing to collapse a pinned item is
/// the projection's job, and it happens where the pin is recorded.
fn collapsible(item: &Item) -> bool {
    item.placement != Placement::Phantom
}

impl Item {
    /// Builds an appended item over `provenance`, placed in the body region.
    pub fn new(id: ItemId, lane: Lane, provenance: Vec<StoreRange>, scope: ScopeId) -> Self {
        Self::with_placement(id, lane, provenance, scope, Placement::Placed(Region::Body))
    }

    /// Builds an item whose placement state is given, not implied.
    pub fn with_placement(
        id: ItemId,
        lane: Lane,
        provenance: Vec<StoreRange>,
        scope: ScopeId,
        placement: Placement,
    ) -> Self {
        let byte_range = enclosing(&provenance);
        Self {
            id,
            lane,
            provenance,
            scope,
            placement,
            byte_range,
        }
    }

    /// Builds a store-only item over `provenance`.
    pub fn store_only(id: ItemId, lane: Lane, provenance: Vec<StoreRange>, scope: ScopeId) -> Self {
        Self::with_placement(id, lane, provenance, scope, Placement::StoreOnly)
    }

    /// Builds a phantom item: collapsed to zero bytes, out of every region.
    pub fn phantom(id: ItemId, lane: Lane, scope: ScopeId) -> Self {
        Self::with_placement(id, lane, Vec::new(), scope, Placement::Phantom)
    }

    /// Immutable identifier of the item.
    pub fn id(&self) -> ItemId {
        self.id
    }

    /// Explicit placement state of the item.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Region the item is placed in, when its state is placed.
    pub fn region(&self) -> Option<Region> {
        self.placement.region()
    }

    /// Accounting units charged for this item: covered bytes, counted once.
    pub fn units(&self) -> u64 {
        covered_units(&self.provenance)
    }

    /// Whether the item is a collapsed placeholder: a phantom, holding no bytes.
    pub fn is_placeholder(&self) -> bool {
        self.placement == Placement::Phantom
    }

    /// Whether the item is store-only: bytes in the store, out of every region.
    pub fn is_store_only(&self) -> bool {
        self.placement == Placement::StoreOnly
    }

    /// Transitions the item to store-only, preserving its bytes.
    pub fn to_store_only(&mut self) {
        self.placement = Placement::StoreOnly;
    }

    /// Collapses the item in place: it becomes a phantom holding no bytes. A
    /// phantom cannot collapse again.
    pub fn collapse(&mut self) -> Result<(), IrError> {
        if !collapsible(self) {
            return Err(IrError::PlacementState {
                id: self.id.value(),
            });
        }
        self.provenance.clear();
        self.byte_range = StoreRange {
            offset: 0,
            length: 0,
        };
        self.placement = Placement::Phantom;
        Ok(())
    }

    /// Encodes the item into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        self.id.encode(sink);
        sink.tag(self.lane.name());
        sink.int(self.scope);
        match self.placement {
            Placement::Placed(region) => region.encode(sink),
            Placement::StoreOnly => sink.tag("store-only"),
            Placement::Phantom => sink.tag("phantom"),
        }
        sink.int(self.byte_range.offset);
        sink.int(self.byte_range.length);
        for range in &self.provenance {
            sink.int(range.offset);
            sink.int(range.length);
        }
    }
}

/// Errors raised by conversation-IR operations.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IrError {
    /// No item carries the identifier.
    UnknownItem {
        /// Identifier that failed to resolve.
        id: u64,
    },
    /// An item with the identifier already exists.
    DuplicateItem {
        /// Duplicated identifier.
        id: u64,
    },
    /// A split was requested with no parts, or with an empty part.
    EmptySplit {
        /// Identifier of the item being split.
        id: u64,
    },
    /// The child ranges do not cover the parent exactly and disjointly.
    CoverageMismatch {
        /// Identifier of the item being split.
        id: u64,
    },
    /// An operation named a namespace discriminant no namespace defines.
    UnknownNamespace {
        /// Discriminant carried by the operation.
        found: u64,
    },
    /// A resegment contract was malformed: the part count, the split points, or
    /// the child ranges did not satisfy the contract.
    ContractMismatch {
        /// Identifier of the item being resegmented.
        id: u64,
    },
    /// A placement operation named a state the item cannot enter from its
    /// current state.
    PlacementState {
        /// Identifier of the item.
        id: u64,
    },
    /// A logged resegment tried to cut a parent whose provenance carries more
    /// than one recorded range: the even slice cannot respect the claims those
    /// ranges record, so the split is refused.
    ClaimBoundary {
        /// Identifier of the item being resegmented.
        id: u64,
    },
    /// An append recorded claims that do not cover its own bytes exactly, so the
    /// claims cannot become items without silently dropping bytes.
    ClaimsDontCover {
        /// Sequence of the offending append event.
        sequence: u64,
    },
}

/// Occupants of one region, in placement order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegionPartition {
    /// Region this partition charges.
    pub region: Region,
    items: Vec<ItemId>,
}

impl RegionPartition {
    /// Items placed in the region, each at most once.
    pub fn items(&self) -> &[ItemId] {
        &self.items
    }
}

/// Whether an item's namespace and provenance are internally consistent: an
/// append identifier always carries provenance, since the append landed bytes on
/// the spine.
fn id_fits_provenance(id: ItemId, provenance: &[StoreRange]) -> bool {
    !matches!(id.namespace(), ItemNamespace::Append) || !provenance.is_empty()
}

/// Namespace a resegment contract mints child identifiers from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitNamespace {
    /// Children mint from the split namespace, independent of append ids.
    Fresh,
    /// Children keep the parent's append namespace. The mint still draws from the
    /// split counter, so an inherited identifier that would collide with a
    /// recorded append is refused instead of silently reused.
    Inherit,
}

/// The resegment contract: the namespace children mint from, the expected part
/// count, and the per-part range counts. A logged resegment event carries the
/// same contract, and a split that does not satisfy it is refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SplitContract {
    /// Namespace the children mint from.
    pub namespace: SplitNamespace,
    /// Expected number of children.
    pub parts: usize,
    /// Expected range count per part.
    pub split_points: Vec<usize>,
}

/// Conversation intermediate representation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConversationIr {
    items: Vec<Item>,
    partitions: Vec<RegionPartition>,
    next_append_id: u64,
    next_split_id: u64,
}

impl ConversationIr {
    /// Creates an empty IR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of items, placed or unplaced.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the IR holds no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// All items in identifier order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Looks up an item by identifier.
    pub fn item(&self, id: ItemId) -> Result<&Item, IrError> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .ok_or(IrError::UnknownItem { id: id.value() })
    }

    /// Region partitions in occupancy order.
    pub fn region_partitions(&self) -> &[RegionPartition] {
        &self.partitions
    }

    /// Identifiers placed in `region`, each at most once.
    pub fn region_items(&self, region: Region) -> Vec<ItemId> {
        match self.partitions.iter().find(|part| part.region == region) {
            Some(part) => part.items.clone(),
            None => Vec::new(),
        }
    }

    /// Identifiers of every placed item, without duplication.
    pub fn placed_ids(&self) -> Vec<ItemId> {
        let mut placed: Vec<ItemId> = Vec::new();
        for partition in &self.partitions {
            placed.extend(partition.items.iter().copied());
        }
        placed
    }

    /// Occupancy charged to `region`, in accounting units.
    pub fn region_occupancy(&self, region: Region) -> u64 {
        self.region_items(region)
            .iter()
            .filter_map(|id| self.item(*id).ok())
            .map(|item| item.units())
            .sum()
    }

    /// Identifiers of items in `lane`.
    pub fn items_in_lane(&self, lane: Lane) -> Vec<ItemId> {
        self.items
            .iter()
            .filter(|item| item.lane == lane)
            .map(|item| item.id())
            .collect()
    }

    /// Total units of items in `lane`, placed or not.
    pub fn lane_units(&self, lane: Lane) -> u64 {
        self.items_in_lane(lane)
            .iter()
            .filter_map(|id| self.item(*id).ok())
            .map(|item| item.units())
            .sum()
    }

    /// Units of items in `lane` that are currently placed.
    pub fn lane_placed_units(&self, lane: Lane) -> u64 {
        self.items_in_lane(lane)
            .iter()
            .filter_map(|id| self.item(*id).ok())
            .filter(|item| item.region().is_some())
            .map(|item| item.units())
            .sum()
    }

    /// Inserts an item; identifiers are unique within their namespace and never
    /// reused there.
    pub fn insert(&mut self, item: Item) -> Result<ItemId, IrError> {
        let id = item.id();
        if self.items.iter().any(|existing| existing.id == id) {
            return Err(IrError::DuplicateItem { id: id.value() });
        }
        if !id_fits_provenance(id, &item.provenance) {
            return Err(IrError::PlacementState { id: id.value() });
        }
        self.note_minted_id(id);
        let region = item.region();
        self.items.push(item);
        if let Some(region) = region {
            self.attach(id, region);
        }
        Ok(id)
    }

    /// Mints the next append identifier; minted identifiers never repeat.
    pub fn reserve_append_id(&mut self) -> ItemId {
        let id = ItemId::append(self.next_append_id);
        self.next_append_id += 1;
        id
    }

    /// Mints the next split identifier, from the namespace independent of appends.
    pub fn reserve_split_id(&mut self) -> ItemId {
        let id = ItemId::split(self.next_split_id);
        self.next_split_id += 1;
        id
    }

    /// Highest identifier value minted per namespace.
    pub fn namespace_watermark(&self, namespace: ItemNamespace) -> u64 {
        match namespace {
            ItemNamespace::Append => self.next_append_id,
            ItemNamespace::Split => self.next_split_id,
        }
    }

    /// Records that `id` was minted, raising the namespace watermark so a later
    /// mint never reuses it.
    pub fn note_minted_id(&mut self, id: ItemId) {
        match id.namespace() {
            ItemNamespace::Append => {
                if id.value() >= self.next_append_id {
                    self.next_append_id = id.value() + 1;
                }
            }
            ItemNamespace::Split => {
                if id.value() >= self.next_split_id {
                    self.next_split_id = id.value() + 1;
                }
            }
        }
    }

    /// Places an item in `region`, moving it out of any previous region so the
    /// item is charged to exactly one region.
    pub fn place(&mut self, id: ItemId, region: Region) -> Result<(), IrError> {
        let item = self.item_mut(id)?;
        item.placement = Placement::Placed(region);
        self.detach(id);
        self.attach(id, region);
        Ok(())
    }

    /// Transitions an item to store-only: out of every region, bytes retained.
    /// The transition is explicit, so it is distinct from a fresh append item, a
    /// collapsed placeholder, and a vaulted redaction.
    pub fn unplace(&mut self, id: ItemId) -> Result<(), IrError> {
        let item = self.item_mut(id)?;
        item.to_store_only();
        self.detach(id);
        Ok(())
    }

    /// Collapses an item to a phantom placeholder: it carries no bytes and sits
    /// out of every region. A phantom cannot collapse again.
    pub fn collapse(&mut self, id: ItemId) -> Result<(), IrError> {
        self.item_mut(id)?.collapse()?;
        self.detach(id);
        Ok(())
    }

    /// Splits an item into claim-atomic children under `contract`. The contract
    /// carries the namespace the children mint from, the expected part count, and
    /// the per-part range counts; a split that does not satisfy it is refused.
    pub fn split(
        &mut self,
        id: ItemId,
        parts: Vec<Vec<StoreRange>>,
        contract: &SplitContract,
    ) -> Result<Vec<ItemId>, IrError> {
        let parent = self.item(id)?.clone();
        if parts.len() != contract.parts || contract.split_points.len() != contract.parts {
            return Err(IrError::ContractMismatch { id: id.value() });
        }
        if parts.is_empty() {
            return Err(IrError::EmptySplit { id: id.value() });
        }
        let mut flat: Vec<StoreRange> = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if part.len() != contract.split_points[index] {
                return Err(IrError::ContractMismatch { id: id.value() });
            }
            if part.is_empty() {
                return Err(IrError::EmptySplit { id: id.value() });
            }
            flat.extend(part.iter().copied());
        }
        if !covers_exactly(&flat, &parent.provenance) {
            return Err(IrError::CoverageMismatch { id: id.value() });
        }
        let parent_claims = normalize(&parent.provenance);
        if parent_claims.len() > 1 {
            let claim_atomic = parts.len() == parent_claims.len()
                && parts.iter().zip(parent_claims.iter()).all(|(part, claim)| {
                    part.len() == 1
                        && part[0].offset == claim.offset
                        && part[0].length == claim.length
                });
            if !claim_atomic {
                return Err(IrError::ClaimBoundary { id: id.value() });
            }
        }
        let region = parent.region();
        let lane = parent.lane;
        let scope = parent.scope;
        self.remove(id);
        let mut children: Vec<ItemId> = Vec::new();
        for part in parts {
            let child = Item::new(self.reserve_split_id(), lane, part, scope);
            if self.items.iter().any(|existing| existing.id == child.id()) {
                return Err(IrError::DuplicateItem {
                    id: child.id().value(),
                });
            }
            children.push(child.id());
            self.items.push(child);
        }
        if let Some(target) = region {
            for child in &children {
                self.attach(*child, target);
            }
        }
        Ok(children)
    }

    /// Encodes the IR into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("conversation-ir");
        sink.int(self.next_append_id);
        sink.int(self.next_split_id);
        for item in &self.items {
            item.encode(sink);
        }
        for partition in &self.partitions {
            sink.int(partition.region.rank());
            sink.int(partition.items.len() as u64);
            for id in &partition.items {
                id.encode(sink);
            }
        }
    }

    fn item_mut(&mut self, id: ItemId) -> Result<&mut Item, IrError> {
        self.items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(IrError::UnknownItem { id: id.value() })
    }

    fn remove(&mut self, id: ItemId) {
        self.items.retain(|item| item.id != id);
        self.detach(id);
    }

    fn detach(&mut self, id: ItemId) {
        for partition in &mut self.partitions {
            partition.items.retain(|other| *other != id);
        }
    }

    fn attach(&mut self, id: ItemId, region: Region) {
        match self
            .partitions
            .iter_mut()
            .find(|part| part.region == region)
        {
            Some(part) => part.items.push(id),
            None => self.partitions.push(RegionPartition {
                region,
                items: vec![id],
            }),
        }
    }
}

/// Whether `children` covers `parent` exactly, with no overlap and no gap.
pub fn covers_exactly(children: &[StoreRange], parent: &[StoreRange]) -> bool {
    let child_units: u64 = children.iter().map(|range| range.length).sum();
    child_units == covered_units(parent) && normalize(children) == normalize(parent)
}
