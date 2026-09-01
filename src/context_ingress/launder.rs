//! Secret-laundering guard for generated artifacts.
//!
//! A generated artifact (fold, summary, digest, read-back answer) re-enters through
//! derivation-ingestion. It may not copy a secret out of a quarantined range: the guard
//! checks the artifact against the digest set of every vaulted plaintext from this
//! transaction, and a generated artifact that carries a quarantined span verbatim is
//! rejected before its durable form is written.

use crate::context_ingress::ingress::IngressRecord;
use crate::context_ingress::segment::is_separator;
use crate::context_kernel::canonical::digest;
use std::collections::HashSet;

/// Result of checking a generated artifact for laundering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunderVerdict {
    /// No quarantined secret material appears in the artifact.
    Clean,
    /// A quarantined span was copied out; the artifact is refused.
    Laundered { secret_digest: u64 },
}

/// Quarantine set: digests of every quarantined span plus the whole quarantined payload.
pub struct QuarantineSet {
    digests: HashSet<u64>,
    spans: Vec<String>,
}

impl QuarantineSet {
    /// Builds the set from the vaulted spans of one committed transaction.
    pub fn from_records(records: &[IngressRecord]) -> Self {
        let mut digests = HashSet::new();
        let mut spans = Vec::new();
        for record in records {
            if let Some(vault) = &record.vault {
                digests.insert(vault.content_digest);
                spans.push(vault.reason.clone());
            }
            for secret in &record.secret_digests {
                digests.insert(*secret);
            }
        }
        Self { digests, spans }
    }

    /// Number of quarantined spans tracked (report only).
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// Checks a generated artifact for copied-out secrets.
    pub fn check(&self, artifact: &[u8]) -> LaunderVerdict {
        let mut start = 0usize;
        for index in 0..=artifact.len() {
            let boundary = index == artifact.len() || is_separator(artifact[index]);
            if boundary {
                if index > start {
                    let token = digest(&artifact[start..index]);
                    if self.digests.contains(&token) {
                        return LaunderVerdict::Laundered {
                            secret_digest: token,
                        };
                    }
                }
                start = index + 1;
            }
        }
        LaunderVerdict::Clean
    }
}
