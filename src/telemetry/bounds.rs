//! Request contract facts only. No prompts, schemas, endpoints or credentials persist.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestContract {
    /// Hash of route/provider/model/settings identity, never the endpoint itself.
    pub identity: String,
    pub serializer_version: u64,
    pub render_version: u64,
    pub tokenizer: Option<String>,
    pub tokenizer_version: Option<String>,
    pub input_accounting: String,
    pub margin_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundTerms {
    pub system: Option<u64>,
    pub tools: Option<u64>,
    pub body: Option<u64>,
    pub framing: Option<u64>,
    pub output_reserve: Option<u64>,
    pub terminal_reserve: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestBound {
    pub contract: RequestContract,
    pub wire_digest: String,
    pub wire_bytes: u64,
    pub terms: BoundTerms,
    /// Full selected projection estimate. `capability` distinguishes tokenizer from bytes.
    pub estimated_input: Option<u64>,
    pub observed_input: Option<u64>,
    pub margin: Option<u64>,
    pub bounded_input: Option<u64>,
    pub context_limit: Option<u64>,
    pub host_memory_cap_bytes: u64,
    pub capability: String,
}

impl RequestBound {
    pub(crate) fn valid(&self) -> bool {
        let digest = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        digest(&self.wire_digest)
            && digest(&self.contract.identity)
            && self.contract.serializer_version > 0
            && self.contract.render_version > 0
            && self.contract.margin_version > 0
            && self
                .contract
                .tokenizer
                .as_ref()
                .is_none_or(|s| s.len() <= 128)
            && self
                .contract
                .tokenizer_version
                .as_ref()
                .is_none_or(|s| s.len() <= 128)
            && self.contract.input_accounting.len() <= 64
            && self.capability.len() <= 64
            && self.bounded_input
                == self
                    .estimated_input
                    .zip(self.margin)
                    .and_then(|(input, margin)| input.checked_add(margin))
    }
}
