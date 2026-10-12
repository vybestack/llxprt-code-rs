//! Private continuation owner. Display projections never rewrite these provider blocks.
use serde::{Deserialize, Serialize};
use serdes_ai::core::{ModelResponse, ModelResponsePart};

/// Exact ordered response only when a provider returned reasoning state. Serialized
/// exclusively into the existing private session store, never a readable transcript.
#[derive(Clone, Serialize, Deserialize)]
pub struct Continuation {
    response: ModelResponse,
    #[serde(deserialize_with = "required_binding")]
    binding: Option<String>,
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("bytes", &self.bytes())
            .finish()
    }
}

impl Continuation {
    pub(crate) fn capture(response: &ModelResponse) -> Option<Self> {
        response
            .parts
            .iter()
            .any(|p| match p {
                ModelResponsePart::Thinking(_) => true,
                ModelResponsePart::Text(t) => t
                    .provider_details
                    .as_ref()
                    .is_some_and(|d| d.contains_key("phase")),
                _ => false,
            })
            .then(|| Self {
                response: response.clone(),
                binding: None,
            })
    }

    pub(crate) fn bind(&mut self, binding: &str) {
        self.binding = Some(binding.into());
    }

    /// Original ordered transport blocks, without display scrubbing or truncation.
    pub fn response(&self) -> ModelResponse {
        let mut response = self.response.clone();
        if let Some(binding) = &self.binding {
            let details = response
                .vendor_details
                .get_or_insert_with(|| serde_json::json!({}));
            details["host_reasoning_binding"] = serde_json::json!(binding);
        }
        response
    }

    /// Serialized state is charged, not just its readable summary.
    pub fn bytes(&self) -> usize {
        serde_json::to_vec(&self.response).map_or(usize::MAX, |s| s.len())
    }

    /// Reject unsafe state rather than altering signed/plaintext provider blocks.
    pub fn validate(&self, secrets: &[String]) -> Result<(), &'static str> {
        let bytes = serde_json::to_string(&self.response)
            .map_err(|_| "reasoning continuation serialization failed")?;
        if bytes.len() > crate::limits::MAX_TURN_ASSISTANT_BYTES {
            return Err("reasoning continuation exceeds assistant byte cap");
        }
        let readable: String = self
            .response
            .parts
            .iter()
            .filter_map(|p| match p {
                ModelResponsePart::Thinking(t) => Some(t.content.as_str()),
                _ => None,
            })
            .collect();
        if crate::redact::scrub_secrets(&readable, secrets) != readable {
            return Err("reasoning continuation contains unsafe readable content; cannot retain unmodified state");
        }
        for secret in secrets.iter().filter(|s| !s.is_empty()) {
            let encoded =
                serde_json::to_string(secret).map_err(|_| "reasoning secret validation failed")?;
            if readable.contains(secret)
                || bytes.contains(secret)
                || bytes.contains(&encoded[1..encoded.len() - 1])
            {
                return Err("reasoning continuation contained a configured secret; cannot safely retain unmodified state");
            }
        }
        Ok(())
    }
}

fn required_binding<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}
