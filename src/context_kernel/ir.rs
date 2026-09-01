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

/// Immutable identifier of an item. Identifiers are never rewritten and never
/// reused: resegmentation mints fresh identifiers for the children.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ItemId(u64);

impl ItemId {
    /// Wraps a raw identifier value.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw identifier value.
    pub fn value(self) -> u64 {
        self.0
    }

    /// Encodes the identifier into `sink`.
    pub fn encode(self, sink: &mut Sink) {
        sink.tag("item");
        sink.int(self.0);
    }
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

/// One claim: a lane, its byte provenance, its scope, and its placed region.
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
    /// Region the claim is placed in, if any.
    region: Option<Region>,
    /// Enclosing byte range of the provenance.
    pub byte_range: StoreRange,
}

impl Item {
    /// Builds an unplaced item over `provenance`.
    pub fn new(id: ItemId, lane: Lane, provenance: Vec<StoreRange>, scope: ScopeId) -> Self {
        let byte_range = enclosing(&provenance);
        Self {
            id,
            lane,
            provenance,
            scope,
            region: None,
            byte_range,
        }
    }

    /// Immutable identifier of the item.
    pub fn id(&self) -> ItemId {
        self.id
    }

    /// Region the item is placed in.
    pub fn region(&self) -> Option<Region> {
        self.region
    }

    /// Accounting units charged for this item: covered bytes, counted once.
    pub fn units(&self) -> u64 {
        covered_units(&self.provenance)
    }

    /// Whether the item is a collapsed placeholder carrying no bytes.
    pub fn is_placeholder(&self) -> bool {
        self.units() == 0
    }

    fn in_region(mut self, region: Option<Region>) -> Self {
        self.region = region;
        self
    }

    /// Encodes the item into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        self.id.encode(sink);
        sink.tag(self.lane.name());
        sink.int(self.scope);
        match self.region {
            Some(region) => region.encode(sink),
            None => sink.tag("unplaced"),
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

/// Conversation intermediate representation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConversationIr {
    items: Vec<Item>,
    partitions: Vec<RegionPartition>,
    next_id: u64,
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

    /// Inserts an item; identifiers are unique and never reused.
    pub fn insert(&mut self, item: Item) -> Result<ItemId, IrError> {
        let id = item.id();
        if self.items.iter().any(|existing| existing.id == id) {
            return Err(IrError::DuplicateItem { id: id.value() });
        }
        if id.value() >= self.next_id {
            self.next_id = id.value() + 1;
        }
        if let Some(region) = item.region {
            let attached = item.id();
            self.items.push(item);
            self.attach(attached, region);
            return Ok(attached);
        }
        self.items.push(item);
        Ok(id)
    }

    /// Mints the next identifier; minted identifiers never repeat.
    pub fn reserve_id(&mut self) -> ItemId {
        let id = ItemId::new(self.next_id);
        self.next_id += 1;
        id
    }

    /// Places an item in `region`, moving it out of any previous region so the
    /// item is charged to exactly one region.
    pub fn place(&mut self, id: ItemId, region: Region) -> Result<(), IrError> {
        let item = self.item_mut(id)?;
        item.region = Some(region);
        self.detach(id);
        self.attach(id, region);
        Ok(())
    }

    /// Removes an item from its region without touching its bytes.
    pub fn unplace(&mut self, id: ItemId) -> Result<(), IrError> {
        let item = self.item_mut(id)?;
        item.region = None;
        self.detach(id);
        Ok(())
    }

    /// Splits an item into claim-atomic children. The children's ranges must cover
    /// the parent exactly and be disjoint, so byte provenance is preserved.
    pub fn split(
        &mut self,
        id: ItemId,
        parts: Vec<Vec<StoreRange>>,
    ) -> Result<Vec<ItemId>, IrError> {
        let parent = self.item(id)?.clone();
        if parts.is_empty() {
            return Err(IrError::EmptySplit { id: id.value() });
        }
        let mut flat: Vec<StoreRange> = Vec::new();
        for part in &parts {
            if part.is_empty() {
                return Err(IrError::EmptySplit { id: id.value() });
            }
            flat.extend(part.iter().copied());
        }
        if !covers_exactly(&flat, &parent.provenance) {
            return Err(IrError::CoverageMismatch { id: id.value() });
        }
        let region = parent.region;
        let lane = parent.lane;
        let scope = parent.scope;
        self.remove(id);
        let mut children: Vec<ItemId> = Vec::new();
        for part in parts {
            let child = Item::new(self.reserve_id(), lane, part, scope).in_region(region);
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
        sink.int(self.next_id);
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
fn covers_exactly(children: &[StoreRange], parent: &[StoreRange]) -> bool {
    let child_units: u64 = children.iter().map(|range| range.length).sum();
    child_units == covered_units(parent) && normalize(children) == normalize(parent)
}
