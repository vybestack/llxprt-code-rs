//! Request-local monotonic transport observations. No endpoint or payload is retained.
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Transport facts for the current invocation only.
#[derive(Clone, Debug, Default)]
pub struct Observation {
    /// Time to response headers (connect plus provider queue); not generation time.
    pub headers_ms: Option<u64>,
    /// Time to first nonempty response body chunk or WebSocket text frame.
    pub first_body_ms: Option<u64>,
    /// Elapsed time to the terminal streamed completion, not EOF or host parsing.
    pub completed_ms: Option<u64>,
    /// Provider-reported reasoning tokens, when present on SSE completion.
    pub reasoning_tokens: Option<u64>,
    /// Provider-reported total tokens, when present on SSE completion.
    pub total_tokens: Option<u64>,
}

/// A per-request collector explicitly propagated to transport reader tasks.
#[derive(Clone)]
pub struct Collector {
    started: Arc<Mutex<Option<Instant>>>,
    observation: Arc<Mutex<Observation>>,
}

tokio::task_local! { static CURRENT: Collector; }

impl Default for Collector {
    fn default() -> Self {
        Self {
            started: Arc::new(Mutex::new(None)),
            observation: Arc::new(Mutex::new(Observation::default())),
        }
    }
}

impl Collector {
    /// Scope collection to this future; dropping a cancelled future leaves facts readable.
    pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }
    /// Read the collector without resetting it.
    pub fn snapshot(&self) -> Observation {
        self.observation
            .lock()
            .expect("transport observation mutex")
            .clone()
    }
}

/// Clone the current collector for a spawned reader, if collection was requested.
pub fn current() -> Option<Collector> {
    CURRENT.try_with(Clone::clone).ok()
}

fn update(f: impl FnOnce(&mut Observation, u64)) {
    let _ = CURRENT.try_with(|collector| {
        let started = *collector.started.lock().expect("transport clock mutex");
        let Some(started) = started else {
            return;
        };
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        f(
            &mut collector
                .observation
                .lock()
                .expect("transport observation mutex"),
            elapsed,
        );
    });
}

/// Start the wire clock immediately before send/connect, excluding request assembly.
pub fn request_sent() {
    let _ = CURRENT.try_with(|collector| {
        collector
            .started
            .lock()
            .expect("transport clock mutex")
            .get_or_insert_with(Instant::now);
    });
}

/// Mark the first received response headers.
pub fn headers() {
    update(|o, elapsed| {
        o.headers_ms.get_or_insert(elapsed);
    });
}
/// Mark the first nonempty response body chunk/frame, never empty/control frames.
pub fn first_body() {
    update(|o, elapsed| {
        o.first_body_ms.get_or_insert(elapsed);
    });
}
/// Retain terminal SSE usage that the folded stream event cannot represent.
pub fn completed_usage(total: Option<u64>, reasoning: Option<u64>) {
    update(|o, elapsed| {
        o.completed_ms = Some(elapsed);
        o.total_tokens = total;
        o.reasoning_tokens = reasoning;
    });
}
