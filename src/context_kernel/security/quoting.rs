//! Task-model environment serialization. JSON escaping prevents delimiter breakout.
use serde::{Deserialize, Serialize};
use serdes_ai::core::messages::ToolReturnContent;
use serdes_ai::core::{ModelRequest, ModelRequestPart};

const BANNER: &str = "environment";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentEnvelope {
    pub provenance: String,
    pub quoted_data: String,
}
impl EnvironmentEnvelope {
    /// Tool/call identity stays in the enclosing typed part and pairing predicate;
    /// the send intent hashes all those fields and the exact quoted payload.
    pub fn new(data: &str) -> Self {
        Self {
            provenance: BANNER.into(),
            quoted_data: data.into(),
        }
    }
    pub fn valid(&self) -> bool {
        self.provenance == BANNER
    }
    pub fn render(&self) -> String {
        serde_json::to_string(self).expect("typed environment envelope")
    }
}
/// Applied to a fresh wire projection only. No legacy-reader or idempotent unwrap path.
pub fn quote_requests(requests: &[ModelRequest]) -> Vec<ModelRequest> {
    let mut quoted = requests.to_vec();
    for part in quoted.iter_mut().flat_map(|r| &mut r.parts) {
        if let ModelRequestPart::ToolReturn(result) = part {
            let data = result
                .content
                .as_text()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    serde_json::to_string(&result.content).expect("typed tool content")
                });
            let envelope = EnvironmentEnvelope::new(&data).render();
            result.content = if result.content.is_error() {
                ToolReturnContent::error(envelope)
            } else {
                ToolReturnContent::json(serde_json::from_str(&envelope).expect("typed envelope"))
            };
        }
    }
    quoted
}
/// Same predicate at intent publication and send admission over the actual bytes.
pub fn legal_quotes(requests: &[ModelRequest]) -> Result<(), super::SecurityError> {
    for part in requests.iter().flat_map(|r| &r.parts) {
        if let ModelRequestPart::ToolReturn(result) = part {
            let envelope: EnvironmentEnvelope = if let Some(value) = result.content.as_json() {
                serde_json::from_value(value.clone()).map_err(|_| super::SecurityError::Quoting)?
            } else {
                serde_json::from_str(
                    result
                        .content
                        .as_text()
                        .ok_or(super::SecurityError::Quoting)?,
                )
                .map_err(|_| super::SecurityError::Quoting)?
            };
            if !envelope.valid() {
                return Err(super::SecurityError::Quoting);
            }
        }
    }
    Ok(())
}
