//! Provenance-bound authorization. A gate establishes support, never authority.
use crate::context_kernel::{
    ledger::{Authority, Origin, Source},
    validation::{LeafStore, ValidationRecord},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adoption {
    pub principal: Source,
    pub candidate_digest: String,
    pub lane: u8,
    pub scope: u64,
}
/// Authenticator belongs to ingress/operator-envelope owner, not a proposer/model.
pub trait Authorization {
    fn adoption_authenticated(&self, adoption: &Adoption) -> bool;
    fn operator_authenticated(&self, source: &Source) -> bool;
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneAuthority {
    pub source: Source,
    pub adoption: Option<Adoption>,
}
impl LaneAuthority {
    pub fn check(
        &self,
        lane: u8,
        scope: u64,
        record: &ValidationRecord,
        store: &dyn LeafStore,
        auth: Option<&dyn Authorization>,
    ) -> bool {
        let minimum = if lane == 1 {
            Authority::User
        } else {
            Authority::Agent
        };
        let source = store.authenticated_leaf(&self.source.leaf);
        let direct = source.is_some_and(|leaf| {
            leaf.is_ingress()
                && leaf.source == self.source
                && leaf.source.scope == scope
                && leaf.authority() >= minimum
                && !record.support.is_empty()
                && record
                    .support
                    .values()
                    .all(|leaves| leaves.contains(&leaf.id))
        });
        let adopted = self.adoption.as_ref().is_some_and(|adoption| {
            adoption.candidate_digest == record.candidate_digest
                && adoption.lane == lane
                && adoption.scope == scope
                && adoption.principal.scope == scope
                && adoption.principal.origin == Origin::Ingress
                && adoption.principal.authority >= minimum
                && auth.is_some_and(|a| a.adoption_authenticated(adoption))
        });
        direct || adopted
    }
    pub fn replay_valid(&self, lane: u8, scope: u64, digest: &str) -> bool {
        let minimum = if lane == 1 {
            Authority::User
        } else {
            Authority::Agent
        };
        let source = self
            .adoption
            .as_ref()
            .map_or(&self.source, |a| &a.principal);
        source.origin == Origin::Ingress
            && source.authority >= minimum
            && source.scope == scope
            && !source.leaf.is_empty()
            && self
                .adoption
                .as_ref()
                .is_none_or(|a| a.lane == lane && a.scope == scope && a.candidate_digest == digest)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub path: String,
    pub digest: String,
    pub scope: u64,
    pub authority: Authority,
}
/// Frozen at session start by the operator. File class/name alone never delegates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedConfig {
    files: BTreeMap<String, Delegation>,
}
impl DelegatedConfig {
    pub fn from_envelope(
        files: Vec<Delegation>,
        operator: &Source,
        auth: &dyn Authorization,
    ) -> Result<Self, SecurityError> {
        if operator.authority != Authority::Operator
            || operator.origin != Origin::Ingress
            || !auth.operator_authenticated(operator)
            || files.len() > 256
        {
            return Err(SecurityError::Authority);
        }
        let mut result = Self::default();
        for file in files {
            if !valid_path(&file.path)
                || !valid_hash(&file.digest)
                || file.scope != operator.scope
                || file.authority > operator.authority
                || result.files.insert(file.path.clone(), file).is_some()
            {
                return Err(SecurityError::Delegation);
            }
        }
        Ok(result)
    }
    pub fn resolve(
        &self,
        path: &str,
        bytes: &[u8],
        scope: u64,
    ) -> Result<Authority, SecurityError> {
        let file = self.files.get(path).ok_or(SecurityError::Delegation)?;
        if file.scope != scope || file.digest != hash(bytes) {
            return Err(SecurityError::Delegation);
        }
        Ok(file.authority)
    }
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecurityError {
    #[error("environment quoting predicate failed")]
    Quoting,
    #[error("authenticated scoped authority required")]
    Authority,
    #[error("configuration is not envelope-named and hash-pinned in scope")]
    Delegation,
    #[error("pin owner, expiry, cap or item protection mismatch")]
    Pin,
    #[error("escalation pending/completion exceeds the operator envelope")]
    Escalation,
    #[error("erasure receipt/checkpoint mismatch")]
    Erasure,
}
/// Protected candidates are a deterministic floor ahead of poisonable rankings.
pub fn retrieval_candidates(
    protected: &BTreeSet<String>,
    ranked: &[String],
    cap: usize,
) -> Result<Vec<String>, SecurityError> {
    if protected.len() > cap {
        return Err(SecurityError::Pin);
    }
    let mut result: Vec<_> = protected.iter().cloned().collect();
    for id in ranked {
        if result.len() >= cap {
            break;
        }
        if !result.contains(id) {
            result.push(id.clone());
        }
    }
    Ok(result)
}
