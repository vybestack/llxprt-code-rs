//! Bounded request observations carried through the backend's existing framed error.

use serdes_ai::models::error::cause::TerminalCause;

/// Measured host provider-call boundary, not time-to-first-byte or tool/runtime time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestObservation {
    /// Typed terminal source category. Unknown never implies a peer diagnosis.
    pub cause: Option<TerminalCause>,
    /// Elapsed milliseconds across the provider call, including local conversion,
    /// send, response read and parse; absent when not measured.
    pub provider_call_ms: Option<u64>,
}

impl RequestObservation {
    pub(super) fn from_fields(fields: &str) -> Option<Self> {
        let mut observation = Self::default();
        for field in fields.split(", ") {
            if let Some(token) = field.strip_prefix("cause ") {
                observation.cause = Some(TerminalCause::from_token(token)?);
            }
            if let Some(value) = field.strip_prefix("provider-call-ms ") {
                observation.provider_call_ms = Some(value.parse().ok()?);
            }
        }
        Some(observation)
    }

    pub(super) fn render(&self, out: &mut String) {
        if let Some(cause) = self.cause {
            out.push_str(&format!(", cause {}", cause.as_str()));
        }
        if let Some(elapsed) = self.provider_call_ms {
            out.push_str(&format!(", provider-call-ms {elapsed}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_round_trip_without_external_text() {
        let observation = RequestObservation {
            cause: Some(TerminalCause::IncompleteMessage),
            provider_call_ms: Some(71),
        };
        let mut out = String::new();
        observation.render(&mut out);
        assert_eq!(RequestObservation::from_fields(&out), Some(observation));
        assert!(RequestObservation::from_fields("cause secret.example").is_none());
        assert!(RequestObservation::from_fields("provider-call-ms secret").is_none());
        assert!(RequestObservation::from_fields("provider-call-ms 18446744073709551616").is_none());
        assert_eq!(
            RequestObservation::from_fields(""),
            Some(RequestObservation::default())
        );
    }
}
