//! Bounded request observations carried on the typed transport failure and rendered into its diagnostic.

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
    fn observations_render_only_typed_tokens_and_measured_numbers() {
        let observation = RequestObservation {
            cause: Some(TerminalCause::IncompleteMessage),
            provider_call_ms: Some(71),
        };
        let mut out = String::new();
        observation.render(&mut out);
        assert_eq!(out, ", cause incomplete-message, provider-call-ms 71");
        let mut empty = String::new();
        RequestObservation::default().render(&mut empty);
        assert_eq!(empty, "");
    }
}
