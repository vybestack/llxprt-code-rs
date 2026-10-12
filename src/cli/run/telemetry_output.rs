//! Explicit create-only JSONL telemetry output; never a session-store artifact.
use crate::jsonl_sink::Sink;
use std::path::Path;

pub(crate) struct Output(Sink);

impl Output {
    pub(crate) fn create(
        path: &Path,
        config: &crate::config::ConfigHomeRoot,
    ) -> Result<Self, String> {
        let parent = Sink::open_parent(path).map_err(|e| diagnostic("open", e))?;
        crate::session::validate_diagnostic_destination(config, path, &parent)
            .map_err(|e| diagnostic("ownership", e))?;
        Sink::create_in(path, parent)
            .map(Self)
            .map_err(|e| diagnostic("open", e))
    }

    pub(crate) fn publish(
        &mut self,
        telemetry: &crate::telemetry::Telemetry,
    ) -> Result<(), String> {
        for call in &telemetry.rounds {
            self.0
                .write_event(&serde_json::json!({"event":"model_call", "telemetry":call}))
                .map_err(|e| diagnostic("write", e))?;
        }
        self.0
            .write_event(&serde_json::json!({"event":"turn", "telemetry":telemetry}))
            .and_then(|()| self.0.sync_file())
            .and_then(|()| self.0.sync_parent())
            .map_err(|e| diagnostic("publish", e))
    }
}

fn diagnostic(stage: &str, error: std::io::Error) -> String {
    format!("telemetry output {stage} failed: {}", error.kind())
}
