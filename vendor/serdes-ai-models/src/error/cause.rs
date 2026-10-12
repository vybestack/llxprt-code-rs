//! Value-free terminal source-chain classification for reqwest failures.
//!
//! No source Display/Debug string is inspected or retained. A bounded walk can only
//! emit these allowlisted categories; unknown chains remain explicitly unknown.

use std::error::Error;

/// Maximum source nodes inspected, including the outer error.
pub const MAX_CAUSE_DEPTH: usize = 32;

/// A typed observation, not a diagnosis of who closed the connection or why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCause {
    /// No recognized typed source was exposed.
    Unknown,
    /// Source traversal hit its fixed bound.
    ChainLimit,
    /// Hyper received an incomplete HTTP message.
    IncompleteMessage,
    /// Hyper reports a closed connection.
    ConnectionClosed,
    /// An I/O source reports a connection reset.
    ConnectionReset,
    /// An I/O source reports a connection abort.
    ConnectionAborted,
    /// An I/O source reports unexpected EOF.
    UnexpectedEof,
    /// An I/O source reports a broken pipe.
    BrokenPipe,
    /// An I/O source reports connection refusal.
    ConnectionRefused,
    /// An I/O source reports a timeout.
    TimedOut,
    /// An I/O source exposes another error kind.
    OtherIo,
    /// A rustls source reports a received TLS alert; alert text is not retained.
    TlsAlert,
    /// A rustls source reports another TLS error; its data is not retained.
    Tls,
}

impl TerminalCause {
    /// Stable public diagnostic token, containing no external text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ChainLimit => "chain-limit",
            Self::IncompleteMessage => "incomplete-message",
            Self::ConnectionClosed => "connection-closed",
            Self::ConnectionReset => "connection-reset",
            Self::ConnectionAborted => "connection-aborted",
            Self::UnexpectedEof => "unexpected-eof",
            Self::BrokenPipe => "broken-pipe",
            Self::ConnectionRefused => "connection-refused",
            Self::TimedOut => "timed-out",
            Self::OtherIo => "other-io",
            Self::TlsAlert => "tls-alert",
            Self::Tls => "tls",
        }
    }

    /// Parse only an allowlisted token, never arbitrary source prose.
    pub fn from_token(token: &str) -> Option<Self> {
        [
            Self::Unknown,
            Self::ChainLimit,
            Self::IncompleteMessage,
            Self::ConnectionClosed,
            Self::ConnectionReset,
            Self::ConnectionAborted,
            Self::UnexpectedEof,
            Self::BrokenPipe,
            Self::ConnectionRefused,
            Self::TimedOut,
            Self::OtherIo,
            Self::TlsAlert,
            Self::Tls,
        ]
        .into_iter()
        .find(|cause| cause.as_str() == token)
    }

    /// Inspect a bounded typed chain. Deeper recognized causes replace outer ones.
    /// A capped/cyclic chain never masquerades as a fully inspected terminal cause.
    pub fn from_error(error: &(dyn Error + 'static)) -> Self {
        let mut source = Some(error);
        let mut cause = Self::Unknown;
        for _ in 0..MAX_CAUSE_DEPTH {
            let Some(error) = source else { return cause };
            if let Some(typed) = typed_cause(error) {
                cause = typed;
            }
            // std::io::Error::source can skip its boxed inner error itself.
            source = match error
                .downcast_ref::<std::io::Error>()
                .and_then(|io| io.get_ref())
            {
                Some(inner) => Some(inner),
                None => error.source(),
            };
        }
        if source.is_some() {
            Self::ChainLimit
        } else {
            cause
        }
    }
}

fn typed_cause(error: &(dyn Error + 'static)) -> Option<TerminalCause> {
    if let Some(error) = error.downcast_ref::<hyper::Error>() {
        return if error.is_incomplete_message() {
            Some(TerminalCause::IncompleteMessage)
        } else if error.is_closed() {
            Some(TerminalCause::ConnectionClosed)
        } else {
            None
        };
    }
    if let Some(error) = error.downcast_ref::<rustls::Error>() {
        return Some(match error {
            rustls::Error::AlertReceived(_) => TerminalCause::TlsAlert,
            _ => TerminalCause::Tls,
        });
    }
    error.downcast_ref::<std::io::Error>().map(|error| {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::ConnectionReset => TerminalCause::ConnectionReset,
            ErrorKind::ConnectionAborted => TerminalCause::ConnectionAborted,
            ErrorKind::UnexpectedEof => TerminalCause::UnexpectedEof,
            ErrorKind::BrokenPipe => TerminalCause::BrokenPipe,
            ErrorKind::ConnectionRefused => TerminalCause::ConnectionRefused,
            ErrorKind::TimedOut => TerminalCause::TimedOut,
            _ => TerminalCause::OtherIo,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Cycle;
    impl std::fmt::Display for Cycle {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("must not inspect external error text")
        }
    }
    impl Error for Cycle {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self)
        }
    }

    #[test]
    fn bounded_walk_never_formats_sources() {
        assert_eq!(TerminalCause::from_error(&Cycle), TerminalCause::ChainLimit);
        let error = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "secret-host/body");
        assert_eq!(
            TerminalCause::from_error(&error),
            TerminalCause::ConnectionReset
        );
        assert_eq!(
            TerminalCause::from_error(&rustls::Error::General("secret".into())),
            TerminalCause::Tls
        );
        assert_eq!(
            TerminalCause::from_error(&rustls::Error::AlertReceived(
                rustls::AlertDescription::InternalError
            )),
            TerminalCause::TlsAlert
        );
        assert!(TerminalCause::from_token("secret").is_none());
        let tls = std::io::Error::other(rustls::Error::General("secret".into()));
        assert_eq!(TerminalCause::from_error(&tls), TerminalCause::Tls);
        for cause in [
            TerminalCause::Unknown,
            TerminalCause::ConnectionReset,
            TerminalCause::TlsAlert,
        ] {
            assert_eq!(TerminalCause::from_token(cause.as_str()), Some(cause));
        }
    }
}
