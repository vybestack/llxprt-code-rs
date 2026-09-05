//! Retryability classification for model transport failures surfaced through the
//! headless stdout error envelope.
//!
//! The vendored `serdes_ai::models::ModelError` already carries a structured,
//! value-free [`serdes_ai::models::error::TransportDetail`] (the failure origin, the URL scheme class, the HTTP
//! status, and a bounded body prefix with its total byte length). This module turns that
//! into the small, stable token the envelope reports, plus the bounded diagnostic text a
//! human reads. No host, port, path, query, or credential ever travels: the underlying
//! detail is value-free by construction, and any retained body prefix is bounded again
//! here and scrubbed by the caller before it reaches stdout or the session store.

use std::time::Duration;

/// The bounded diagnostic prefix retained from a provider response body. The provider
/// body was already capped by the vendored bounded reader; this bounds the slice the
/// host renders again, matching the repo's value-free bounded error style.
pub const MAX_TRANSPORT_BODY_PREFIX_BYTES: usize =
    serdes_ai::models::error::MAX_TRANSPORT_BODY_PREFIX_BYTES;

/// The maximum bytes of the rendered transport diagnostic before the caller's own
/// scrub-and-bound applies. Kept well under
/// [`crate::redact::MAX_DIAGNOSTIC_BYTES`] so a bounded body prefix plus the
/// classification framing can never produce an unbounded stdout field.
pub const MAX_TRANSPORT_DIAGNOSTIC_BYTES: usize = 1024;

/// The retryability classification reported in the error envelope. A driver reads the
/// `error.code` field (for example `model-quota-exhausted`) instead of matching on
/// message text.
///
/// This states a fact about the failure. It is deliberately not a retry policy: the
/// retry and backoff behavior is owned separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportKind {
    /// A provider throttle: HTTP 429 without quota or billing wording. Retryable.
    RateLimit,
    /// A quota or billing hard stop (a weekly usage cap, `insufficient_quota`). Not
    /// retryable: backoff cannot clear it.
    QuotaExhausted,
    /// A transient provider-side failure (5xx). Retryable.
    TransientServer,
    /// A connection-level failure: connect, timeout, or a dropped body. Retryable.
    Connectivity,
    /// A definitive 4xx refusal (authorization or request shape). Not retryable.
    Permanent,
}

impl TransportKind {
    /// Whether an identical request could still succeed.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::TransientServer | Self::Connectivity
        )
    }

    /// The stable token used in the envelope error code.
    pub fn token(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::QuotaExhausted => "quota-exhausted",
            Self::TransientServer => "transient-server",
            Self::Connectivity => "connectivity",
            Self::Permanent => "permanent",
        }
    }

    /// The stable envelope error-code component for this kind: the model code plus the
    /// classification token, for example `model-quota-exhausted`.
    pub fn key(self) -> &'static str {
        match self {
            Self::RateLimit => "model-rate-limit",
            Self::QuotaExhausted => "model-quota-exhausted",
            Self::TransientServer => "model-transient-server",
            Self::Connectivity => "model-connectivity",
            Self::Permanent => "model-permanent",
        }
    }

    /// Map the vendored classification onto the host token.
    pub fn from_class(class: serdes_ai::models::error::TransportClass) -> Self {
        match class {
            serdes_ai::models::error::TransportClass::RateLimit => Self::RateLimit,
            serdes_ai::models::error::TransportClass::QuotaExhausted => Self::QuotaExhausted,
            serdes_ai::models::error::TransportClass::TransientServer => Self::TransientServer,
            serdes_ai::models::error::TransportClass::Connectivity => Self::Connectivity,
            serdes_ai::models::error::TransportClass::Permanent => Self::Permanent,
        }
    }
}

/// The classified transport failure the agent and the envelope share.
#[derive(Clone, Debug)]
pub struct TransportFailure {
    /// The retryability classification.
    pub kind: TransportKind,
    /// Where the failure happened, as a value-free token.
    pub origin: &'static str,
    /// The URL scheme class, as a value-free token, when reqwest exposed a URL.
    pub url_class: Option<&'static str>,
    /// The HTTP status, when a response exists.
    pub status: Option<u16>,
    /// The bounded provider body prefix, when a response body exists.
    pub body_prefix: Option<String>,
    /// The total provider body byte length the prefix was taken from.
    pub body_bytes: usize,
    /// The provider's `Retry-After` hint, when one was sent.
    pub retry_after: Option<Duration>,
}

impl TransportFailure {
    /// Extract the classification from a vendored model error. `None` when the error is
    /// not a transport-layer failure.
    pub fn from_model_error(error: &serdes_ai::models::ModelError) -> Option<Self> {
        use serdes_ai::models::ModelError;
        let (kind, origin, url_class, status, body_prefix, body_bytes, retry_after) = match error {
            ModelError::Transport(detail) => return Some(Self::from_detail(detail)),
            ModelError::Timeout => (
                TransportKind::Connectivity,
                "timeout",
                None,
                None,
                None,
                0,
                None,
            ),
            ModelError::Connection(_) => (
                TransportKind::Connectivity,
                "connect",
                None,
                None,
                None,
                0,
                None,
            ),
            ModelError::Network(_) => (
                TransportKind::Connectivity,
                "unknown",
                None,
                None,
                None,
                0,
                None,
            ),
            ModelError::RateLimited { retry_after } => (
                TransportKind::RateLimit,
                "status",
                None,
                Some(429),
                None,
                0,
                *retry_after,
            ),
            ModelError::Http { status, body, .. } => (
                TransportKind::from_class(serdes_ai::models::error::classify_status(*status, body)),
                "status",
                None,
                Some(*status),
                if body.is_empty() {
                    None
                } else {
                    Some(bounded_prefix(body))
                },
                body.len(),
                None,
            ),
            ModelError::Authentication(_) => (
                TransportKind::Permanent,
                "status",
                None,
                Some(401),
                None,
                0,
                None,
            ),
            _ => return None,
        };
        Some(Self {
            kind,
            origin,
            url_class,
            status,
            body_prefix,
            body_bytes,
            retry_after,
        })
    }

    /// Extract the classification from a vendored structured transport detail.
    fn from_detail(detail: &serdes_ai::models::error::TransportDetail) -> Self {
        let status = match detail.origin {
            serdes_ai::models::error::TransportOrigin::Status(status) => Some(status),
            _ => None,
        };
        Self {
            kind: TransportKind::from_class(detail.classify()),
            origin: detail_origin(detail.origin),
            url_class: detail.url_class.map(|class| class.as_str()),
            status,
            body_prefix: detail.body_prefix.clone(),
            body_bytes: detail.body_bytes,
            retry_after: detail.retry_after,
        }
    }

    /// Extract the classification from a backend error message. The backend renders a
    /// transport failure as its diagnostic text, which this recognizes by its stable
    /// framing so the envelope can report the classification without a second channel.
    ///
    /// Known limitation: [`crate::adapter::ChatBackend`] still carries `String`
    /// errors, so the round trip here re-parses the host's own framed diagnostic prose
    /// rather than a typed failure. Restructuring the backend onto a typed error is
    /// deliberately out of scope for this change; the framing is fixed and stable, and
    /// anything unrecognized falls back to the plain model error.
    pub fn from_message(message: &str) -> Option<Self> {
        Self::from_framed(message).or_else(|| {
            Self::legacy_message(message).map(|kind| Self {
                kind,
                origin: "unknown",
                url_class: None,
                status: None,
                body_prefix: None,
                body_bytes: 0,
                retry_after: None,
            })
        })
    }

    /// Recognize the structured framing the host backends render for a transport
    /// failure. The framing is a fixed prefix carrying the classification facts, so the
    /// envelope reports the class without matching on diagnostic prose.
    ///
    /// Known limitation, accepted for this change: [`crate::adapter::ChatBackend`]
    /// still carries `String` errors and the backends flatten a classified failure to
    /// its diagnostic text, so this re-parses the host's own framed prose instead of a
    /// typed failure. The framing is fixed and stable, and anything unrecognized falls
    /// back to the plain model error; restructuring the backend onto a typed error is
    /// deliberately out of scope here.
    fn from_framed(message: &str) -> Option<Self> {
        const PREFIX: &str = "model transport failed (";
        let rest = message.strip_prefix(PREFIX)?;
        let end = rest.find(')')?;
        let fields = &rest[..end];
        let tail = &rest[end + 1..];
        let mut status = None;
        let mut origin = "unknown";
        let mut url_class = None;
        for field in fields.split(", ") {
            if let Some(value) = field.strip_prefix("status ") {
                status = value.parse::<u16>().ok();
            } else if let Some(value) = field.strip_prefix("origin ") {
                origin = static_origin(value)?;
            } else if let Some(value) = field.strip_prefix("url ") {
                url_class = Some(static_url_class(value)?);
            }
        }
        let kind = fields
            .split_once("class ")
            .and_then(|(_, rest)| rest.split_once(','))
            .map(|(token, _)| token)
            .and_then(kind_from_token)?;
        // The body prefix ends at the `retry-after` field when one is present.
        let body_prefix = tail.split_once(" body[").and_then(|(_, rest)| {
            rest.split_once("]: ")
                .map(|(_, body)| match body.split_once(" retry-after ") {
                    Some((prefix, _)) => prefix.to_string(),
                    None => body.to_string(),
                })
        });
        let retry_after = tail
            .split_once(" retry-after ")
            .and_then(|(_, rest)| rest.strip_suffix('s'))
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        Some(Self {
            kind,
            origin,
            url_class,
            status,
            body_prefix,
            body_bytes: 0,
            retry_after,
        })
    }

    /// Recognize the transport sentences the vendored crate and the host backends have
    /// always emitted, so pre-existing failures classify too.
    fn legacy_message(message: &str) -> Option<TransportKind> {
        if message.contains("rate limited") {
            Some(TransportKind::RateLimit)
        } else if message.contains("connection failed")
            || message.contains("network request failed")
            || message.contains("timed out")
            || message.contains("transport failed")
        {
            Some(TransportKind::Connectivity)
        } else {
            None
        }
    }

    /// The stable `error.code` component for this failure: the model code plus the
    /// classification token, for example `model-quota-exhausted`.
    pub fn transport_key(&self) -> &'static str {
        self.kind.key()
    }

    /// The bounded human diagnostic. It states the status, the origin, the URL scheme
    /// class, the classification with its retryability verdict, the bounded body prefix
    /// with its total byte length, and the `Retry-After` hint when one exists. Long
    /// bodies are truncated with the repo's `[truncated]` marker so the total stays
    /// bounded.
    pub fn diagnostic(&self) -> String {
        let mut out = String::new();
        match self.status {
            Some(status) => out.push_str(&format!(
                "model transport failed (status {status}, origin {}",
                self.origin
            )),
            None => out.push_str(&format!("model transport failed (origin {}", self.origin)),
        }
        if let Some(url_class) = self.url_class {
            out.push_str(&format!(", url {url_class}"));
        }
        let verdict = if self.kind.is_retryable() {
            "retryable"
        } else {
            "not retryable"
        };
        out.push_str(&format!(", class {}, {verdict})", self.kind.token()));
        if let Some(prefix) = &self.body_prefix {
            let shown = bound_diagnostic(prefix);
            out.push_str(&format!(
                " body[{}/{} bytes]: {shown}",
                shown.len(),
                self.body_bytes
            ));
        }
        if let Some(retry_after) = self.retry_after {
            out.push_str(&format!(" retry-after {}s", retry_after.as_secs()));
        }
        out
    }
}

/// Map an origin token back to its static string.
fn static_origin(token: &str) -> Option<&'static str> {
    const TOKENS: [&str; 8] = [
        "connect", "timeout", "request", "body", "decode", "redirect", "status", "unknown",
    ];
    TOKENS.into_iter().find(|candidate| *candidate == token)
}

/// Map a URL scheme-class token back to its static string.
fn static_url_class(token: &str) -> Option<&'static str> {
    const TOKENS: [&str; 3] = ["https", "http", "other"];
    TOKENS.into_iter().find(|candidate| *candidate == token)
}

/// Map a classification token back to its kind.
fn kind_from_token(token: &str) -> Option<TransportKind> {
    match token {
        "rate-limit" => Some(TransportKind::RateLimit),
        "quota-exhausted" => Some(TransportKind::QuotaExhausted),
        "transient-server" => Some(TransportKind::TransientServer),
        "connectivity" => Some(TransportKind::Connectivity),
        "permanent" => Some(TransportKind::Permanent),
        _ => None,
    }
}

/// Map a vendored failure origin onto its value-free token.
fn detail_origin(origin: serdes_ai::models::error::TransportOrigin) -> &'static str {
    origin.as_str()
}

/// Bound a provider body prefix for rendering.
fn bounded_prefix(body: &str) -> String {
    if body.len() <= MAX_TRANSPORT_BODY_PREFIX_BYTES {
        return body.to_string();
    }
    let mut end = MAX_TRANSPORT_BODY_PREFIX_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_string()
}

/// Bound the rendered diagnostic text, appending the repo's truncation marker.
fn bound_diagnostic(text: &str) -> String {
    crate::redact::truncate_utf8(text.to_string(), MAX_TRANSPORT_DIAGNOSTIC_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_failure(status: u16, body: &str, retry_after: Option<Duration>) -> TransportFailure {
        TransportFailure {
            kind: TransportKind::from_class(serdes_ai::models::error::classify_status(
                status, body,
            )),
            origin: "status",
            url_class: None,
            status: Some(status),
            body_prefix: if body.is_empty() {
                None
            } else {
                Some(body.to_string())
            },
            body_bytes: body.len(),
            retry_after,
        }
    }

    fn bare_failure(kind: TransportKind, origin: &'static str) -> TransportFailure {
        TransportFailure {
            kind,
            origin,
            url_class: None,
            status: None,
            body_prefix: None,
            body_bytes: 0,
            retry_after: None,
        }
    }

    /// The vendored status classification, driven through the host kinds: quota wording
    /// wins, 429 throttles, 408 and 5xx retry, and the remaining 4xx refuse.
    #[test]
    fn status_codes_classify_retryability() {
        let cases = [
            (429u16, "rate limit", TransportKind::RateLimit),
            (429, "usage limit reached", TransportKind::QuotaExhausted),
            (429, "insufficient_quota", TransportKind::QuotaExhausted),
            (402, "", TransportKind::QuotaExhausted),
            (
                403,
                "exceeded your current quota",
                TransportKind::QuotaExhausted,
            ),
            (408, "", TransportKind::Connectivity),
            (500, "", TransportKind::TransientServer),
            (503, "overloaded", TransportKind::TransientServer),
            (400, "bad request", TransportKind::Permanent),
            (401, "", TransportKind::Permanent),
            (404, "", TransportKind::Permanent),
            (422, "", TransportKind::Permanent),
        ];
        for (status, body, expected) in cases {
            let rendered = status_failure(status, body, None);
            assert_eq!(rendered.kind, expected, "status {status} body {body}");
            assert_eq!(rendered.transport_key(), expected.key());
        }
    }

    /// The retryability verdict and the envelope key agree for every kind.
    #[test]
    fn retryability_and_envelope_keys_agree() {
        for kind in [
            TransportKind::RateLimit,
            TransportKind::TransientServer,
            TransportKind::Connectivity,
        ] {
            assert!(kind.is_retryable());
            assert!(kind.key().starts_with("model-"));
        }
        for kind in [TransportKind::QuotaExhausted, TransportKind::Permanent] {
            assert!(!kind.is_retryable());
            assert!(kind.key().starts_with("model-"));
        }
        assert_eq!(TransportKind::QuotaExhausted.key(), "model-quota-exhausted");
        assert_eq!(TransportKind::RateLimit.key(), "model-rate-limit");
        assert_eq!(
            TransportKind::TransientServer.key(),
            "model-transient-server"
        );
        assert_eq!(TransportKind::Connectivity.key(), "model-connectivity");
        assert_eq!(TransportKind::Permanent.key(), "model-permanent");
    }

    /// The diagnostic states the status, the classification, the bounded body prefix with
    /// its total length, and the `Retry-After` hint when one exists.
    #[test]
    fn diagnostic_states_status_class_body_and_retry_after() {
        let throttled = status_failure(429, "slow down", Some(Duration::from_secs(7)));
        assert_eq!(
            throttled.diagnostic(),
            "model transport failed (status 429, origin status, class rate-limit, retryable) body[9/9 bytes]: slow down retry-after 7s"
        );
        assert_eq!(throttled.transport_key(), "model-rate-limit");

        let refused = status_failure(400, "bad request shape", None);
        assert_eq!(
            refused.diagnostic(),
            "model transport failed (status 400, origin status, class permanent, not retryable) body[17/17 bytes]: bad request shape"
        );
        assert_eq!(refused.transport_key(), "model-permanent");

        let timed_out = bare_failure(TransportKind::Connectivity, "timeout");
        assert_eq!(
            timed_out.diagnostic(),
            "model transport failed (origin timeout, class connectivity, retryable)"
        );
        assert_eq!(timed_out.transport_key(), "model-connectivity");
    }

    /// A long provider body prefix is truncated with the repo's marker so the rendered
    /// diagnostic stays bounded.
    #[test]
    fn diagnostic_bounds_a_long_body_prefix_with_truncation_marker() {
        let body = "y".repeat(MAX_TRANSPORT_DIAGNOSTIC_BYTES * 4);
        let total = body.len();
        let rendered = TransportFailure {
            body_bytes: total,
            body_prefix: Some(body),
            ..status_failure(503, "", None)
        };
        let diagnostic = rendered.diagnostic();
        assert!(
            diagnostic.contains("class transient-server"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("/{total} bytes]: ")),
            "the total body length must be stated: {diagnostic}"
        );
        assert!(
            diagnostic.contains("[truncated]"),
            "must carry the marker: {diagnostic}"
        );
        assert!(
            diagnostic.len() <= MAX_TRANSPORT_DIAGNOSTIC_BYTES + 128,
            "diagnostic must stay bounded: {}",
            diagnostic.len()
        );
    }

    /// The framed host rendering round trips back into the classification the envelope
    /// reports, and legacy prose still classifies.
    #[test]
    fn framed_and_legacy_messages_classify() {
        let rendered = status_failure(429, "slow down", Some(Duration::from_secs(3)));
        let round = TransportFailure::from_message(&rendered.diagnostic())
            .expect("the framed diagnostic must classify");
        assert_eq!(round.kind, TransportKind::RateLimit);
        assert_eq!(round.status, Some(429));
        assert_eq!(round.origin, "status");
        assert_eq!(round.retry_after, Some(Duration::from_secs(3)));
        // The framed body prefix ends where the `retry-after` field begins.
        assert_eq!(round.body_prefix.as_deref(), Some("slow down"));
        assert_eq!(round.transport_key(), "model-rate-limit");

        let legacy = TransportFailure::from_message("Model network request failed")
            .expect("legacy connectivity prose must classify");
        assert_eq!(legacy.kind, TransportKind::Connectivity);
        assert_eq!(legacy.origin, "unknown");
        assert!(TransportFailure::from_message("Model operation failed").is_none());
    }

    /// The vendored variants classify through the same host kinds.
    #[test]
    fn model_error_variants_classify() {
        let detail = serdes_ai::models::error::TransportDetail::from_status(
            502,
            Some("gateway".to_string()),
            7,
            None,
        );
        let mapped =
            TransportFailure::from_model_error(&serdes_ai::models::ModelError::Transport(detail))
                .expect("a transport detail must classify");
        assert_eq!(mapped.kind, TransportKind::TransientServer);
        assert_eq!(mapped.status, Some(502));
        assert_eq!(mapped.transport_key(), "model-transient-server");

        let timed_out = TransportFailure::from_model_error(&serdes_ai::models::ModelError::Timeout)
            .expect("a timeout must classify");
        assert_eq!(timed_out.kind, TransportKind::Connectivity);
        assert_eq!(timed_out.origin, "timeout");
        assert_eq!(timed_out.transport_key(), "model-connectivity");
    }
}
