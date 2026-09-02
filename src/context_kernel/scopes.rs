//! Scope registry: identifiers stable across restarts, lifecycle, nesting, idleness.

use crate::context_kernel::canonical::Sink;

/// Scope identifier. It is the sequence number of the logged scope-open event, so
/// the identifier is resolved from the log and therefore stable across restarts.
pub type ScopeId = u64;

/// Lifecycle state of a scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeState {
    /// Opened and not closed.
    Open,
    /// Closed by an admitted harness scope event.
    ClosedByEvent,
    /// Closed by the scope-close-by-declaration operation.
    ClosedByDeclaration,
}

impl ScopeState {
    /// Stable name used in canonical encodings.
    pub fn name(self) -> &'static str {
        match self {
            ScopeState::Open => "open",
            ScopeState::ClosedByEvent => "closed-by-event",
            ScopeState::ClosedByDeclaration => "closed-by-declaration",
        }
    }

    /// Whether the scope is still open.
    pub fn is_open(self) -> bool {
        self == ScopeState::Open
    }
}

/// One scope with its lineage and log-derived activity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scope {
    /// Identifier resolved from the scope-open event.
    pub id: ScopeId,
    /// Parent scope, when nested.
    pub parent: Option<ScopeId>,
    /// Lifecycle state.
    pub state: ScopeState,
    /// Log sequence of the scope-open event.
    pub opened_sequence: u64,
    /// Log sequence of the closing event, when closed.
    pub closed_sequence: Option<u64>,
    /// Log sequence of the most recent item attributed to this scope.
    pub last_item_sequence: u64,
}

/// Errors raised by scope operations.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScopeError {
    /// No scope with the given identifier.
    UnknownScope { id: ScopeId },
    /// The scope is already closed, so it cannot be closed again.
    AlreadyClosed { id: ScopeId },
    /// The referenced parent scope does not exist.
    UnknownParent { id: ScopeId },
    /// Items cannot be attributed to a closed scope.
    ClosedScope { id: ScopeId },
}

/// Registry of scopes owned by the IR boundary.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeRegistry {
    scopes: Vec<Scope>,
    idleness_window: u64,
}

impl ScopeRegistry {
    /// Creates a registry whose idleness predicate reads a window of `idleness_window`
    /// log events.
    pub fn new(idleness_window: u64) -> Self {
        Self {
            scopes: Vec::new(),
            idleness_window,
        }
    }

    /// Number of registered scopes.
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// Window used by the idleness predicate.
    pub fn idleness_window(&self) -> u64 {
        self.idleness_window
    }

    /// Opens a scope. `id` is the log sequence of the scope-open event; `parent`
    /// is `None` for top-level scopes.
    pub fn open(
        &mut self,
        id: ScopeId,
        parent: Option<ScopeId>,
        at_sequence: u64,
    ) -> Result<(), ScopeError> {
        if let Some(parent_id) = parent {
            self.scope(parent_id)?;
        }
        if self.scopes.iter().any(|scope| scope.id == id) {
            return Ok(());
        }
        self.scopes.push(Scope {
            id,
            parent,
            state: ScopeState::Open,
            opened_sequence: at_sequence,
            closed_sequence: None,
            last_item_sequence: at_sequence,
        });
        Ok(())
    }

    /// Transitions a scope to [`ScopeState::ClosedByEvent`]. Open children remain
    /// open and their items are unaffected; only fold eligibility changes.
    pub fn close_by_event(&mut self, id: ScopeId, at_sequence: u64) -> Result<(), ScopeError> {
        self.close(id, at_sequence, ScopeState::ClosedByEvent)
    }

    /// Transitions a scope to [`ScopeState::ClosedByDeclaration`].
    pub fn close_by_declaration(
        &mut self,
        id: ScopeId,
        at_sequence: u64,
    ) -> Result<(), ScopeError> {
        self.close(id, at_sequence, ScopeState::ClosedByDeclaration)
    }

    /// Attributes an item to `id` at log sequence `at_sequence`.
    pub fn attribute_item(&mut self, id: ScopeId, at_sequence: u64) -> Result<(), ScopeError> {
        let scope = self
            .scopes
            .iter_mut()
            .find(|scope| scope.id == id)
            .ok_or(ScopeError::UnknownScope { id })?;
        if !scope.state.is_open() {
            return Err(ScopeError::ClosedScope { id });
        }
        scope.last_item_sequence = at_sequence;
        Ok(())
    }

    /// Looks up a scope.
    pub fn scope(&self, id: ScopeId) -> Result<&Scope, ScopeError> {
        self.scopes
            .iter()
            .find(|scope| scope.id == id)
            .ok_or(ScopeError::UnknownScope { id })
    }

    /// Lifecycle state of a scope.
    pub fn state(&self, id: ScopeId) -> Result<ScopeState, ScopeError> {
        Ok(self.scope(id)?.state)
    }

    /// Direct children of `id`.
    pub fn children(&self, id: ScopeId) -> Vec<ScopeId> {
        self.scopes
            .iter()
            .filter(|scope| scope.parent == Some(id))
            .map(|scope| scope.id)
            .collect()
    }

    /// Scope idleness over the log window: no item referencing the scope within the
    /// last `idleness_window` log events. The predicate never reads a projection, so
    /// reclamation cannot manufacture idleness.
    pub fn is_idle(&self, id: ScopeId, current_sequence: u64) -> Result<bool, ScopeError> {
        let scope = self.scope(id)?;
        if !scope.state.is_open() {
            return Ok(false);
        }
        Ok(current_sequence > scope.last_item_sequence + self.idleness_window)
    }

    /// Encodes the registry into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("scope-registry");
        sink.int(self.idleness_window);
        for scope in &self.scopes {
            sink.int(scope.id);
            match scope.parent {
                Some(parent) => sink.int(parent),
                None => sink.int(0),
            }
            sink.tag(scope.state.name());
            sink.int(scope.opened_sequence);
            match scope.closed_sequence {
                Some(sequence) => sink.int(sequence),
                None => sink.int(0),
            }
            sink.int(scope.last_item_sequence);
        }
    }

    fn close(
        &mut self,
        id: ScopeId,
        at_sequence: u64,
        state: ScopeState,
    ) -> Result<(), ScopeError> {
        let scope = self
            .scopes
            .iter_mut()
            .find(|scope| scope.id == id)
            .ok_or(ScopeError::UnknownScope { id })?;
        if !scope.state.is_open() {
            return Err(ScopeError::AlreadyClosed { id });
        }
        scope.state = state;
        scope.closed_sequence = Some(at_sequence);
        Ok(())
    }
}
