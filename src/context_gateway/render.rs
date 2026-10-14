//! Version-scoped render publication and identity. Tools are supplied as complete
//! serialized declarations, not a caller-invented size; management bypasses this path.
use serde::{Deserialize, Serialize};
use serdes_ai::core::{ModelRequest, ModelRequestPart, ModelResponsePart};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub version: u64,
    pub serializer_version: u64,
    pub tokenizer_version: Option<String>,
    pub margin_version: u64,
    pub placeholders: bool,
    pub provider_instructions_hash: String,
    pub profile_hash: String,
    pub max_output_tokens: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderPublication {
    pub capability: Capability,
    pub tools_hash: String,
    pub contract_hash: String,
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl RenderPublication {
    pub fn new(capability: Capability, tools: &[u8]) -> Result<Self, super::GatewayError> {
        if capability.version == 0
            || capability.serializer_version == 0
            || capability.margin_version == 0
        {
            return Err(super::GatewayError::Identity);
        }
        let tools_hash = digest(tools);
        let bytes = serde_json::to_vec(&(&capability, &tools_hash))
            .map_err(|_| super::GatewayError::Identity)?;
        Ok(Self {
            capability,
            tools_hash,
            contract_hash: digest(&bytes),
        })
    }
    /// Hash commits to contract, full request parts, declarations and context version.
    /// Erased payloads cannot be substituted at this boundary; a new contract/checkpoint
    /// must be published before their replacement can acquire a different hash.
    pub fn validate(&self) -> Result<(), super::GatewayError> {
        let bytes = serde_json::to_vec(&(&self.capability, &self.tools_hash))
            .map_err(|_| super::GatewayError::Identity)?;
        if !super::protocol::hash_valid(&self.tools_hash)
            || digest(&bytes) != self.contract_hash
            || self.capability.version == 0
            || self.capability.serializer_version == 0
            || self.capability.margin_version == 0
        {
            return Err(super::GatewayError::Identity);
        }
        Ok(())
    }
    pub fn request_hash(
        &self,
        requests: &[ModelRequest],
        version: u64,
    ) -> Result<String, super::GatewayError> {
        let bytes =
            serde_json::to_vec(&("context-request", &self.contract_hash, version, requests))
                .map_err(|_| super::GatewayError::Identity)?;
        Ok(digest(&bytes))
    }
}
/// Structural call/result legality at the actual send boundary. The typed kernel
/// remains the region/lane legality authority; this checks provider pairing/order.
pub fn legal_requests(requests: &[ModelRequest]) -> Result<(), super::GatewayError> {
    let mut pending = std::collections::BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for part in requests.iter().flat_map(|r| &r.parts) {
        match part {
            ModelRequestPart::ModelResponse(response) => {
                if !pending.is_empty() {
                    return Err(super::GatewayError::Transition);
                }
                for piece in &response.parts {
                    if let ModelResponsePart::ToolCall(call) = piece {
                        let id = call
                            .tool_call_id
                            .as_deref()
                            .ok_or(super::GatewayError::Identity)?;
                        if id.is_empty() || !seen.insert(id) {
                            return Err(super::GatewayError::Identity);
                        }
                        pending.insert(id, call.tool_name.as_str());
                    }
                }
            }
            ModelRequestPart::ToolReturn(result) => {
                let id = result
                    .tool_call_id
                    .as_deref()
                    .ok_or(super::GatewayError::Identity)?;
                if pending.remove(id) != Some(result.tool_name.as_str()) {
                    return Err(super::GatewayError::Transition);
                }
            }
            _ if !pending.is_empty() => return Err(super::GatewayError::Transition),
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(super::GatewayError::Transition);
    }
    Ok(())
}
