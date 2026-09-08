use crate::envelope::Code;

/// A look-back error, translated to the standard envelope by the CLI boundary.
pub struct Error {
    pub code: Code,
    pub key: &'static str,
    pub message: String,
}

impl Error {
    fn new(code: Code, key: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            key,
            message: message.into(),
        }
    }
}

use crate::session::{SessionId, SessionStore};
use clap::{Args, Subcommand};
use serde_json::json;
use std::fmt::Write;

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Read persisted turns and rounds, without creating or repairing session files.
    Transcript(Options),
}

#[derive(Debug, Clone, Args)]
pub struct Options {
    /// Session identifier to read. The session must already exist.
    #[arg(long)]
    pub session: String,
    /// Select a 1-based turn, including all its branch attempts.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub turn: Option<u32>,
    /// Print exactly one JSON object with complete persisted text, args and results.
    #[arg(long)]
    pub json: bool,
    /// Maximum bytes displayed per result in human output only; never changes storage.
    #[arg(long, default_value_t = 4096)]
    pub max_bytes: usize,
}

impl Options {
    /// Read the manifest-selected state and render after releasing its read lock.
    pub fn render(&self) -> Result<String, Error> {
        let session =
            SessionId::parse(&self.session).map_err(|e| Error::new(Code::Usage, "session", e))?;
        let root = crate::config::std_profile_dir()
            .map_err(|e| Error::new(Code::Config, "config-home", e))?;
        let state = SessionStore::read_transcript_at(&session, &root)
            .map_err(|e| Error::new(Code::Session, "transcript", e.to_string()))?;
        let branches: Vec<_> = state
            .branches
            .iter()
            .filter(|branch| self.turn.is_none_or(|turn| turn == branch.turn))
            .collect();
        if self.turn.is_some() && branches.is_empty() {
            return Err(Error::new(
                Code::Session,
                "transcript",
                "turn does not exist",
            ));
        }
        if self.json {
            let turns: Vec<_> = branches.iter().map(|branch| {
                let rounds: Vec<_> = branch.rounds.iter().enumerate().map(|(index, round)| {
                    let calls: Vec<_> = round.calls.iter().enumerate().map(|(index, call)| json!({
                        "id":call.id,"name":call.name,"args":call.args,"index":index,"of":round.calls.len(),
                        "ok":call.ok,"refused":call.refused,"result":call.result
                    })).collect();
                    json!({"round":index+1,"text":round.assistant,"calls":calls})
                }).collect();
                json!({"turn":branch.turn,"branch_id":branch.branch_id,"attempt":branch.attempt,
                    "parent_branch":branch.parent_branch,"parent_turn":branch.parent_turn,"parent_attempt":branch.parent_attempt,
                    "prompt":branch.prompt,"lifecycle":branch.lifecycle,"summary":branch.summary,"error":branch.error,"rounds":rounds})
            }).collect();
            return Ok(format!(
                "{}\n",
                json!({"session_id":state.session_id,"turns":turns})
            ));
        }
        let mut output = String::new();
        for branch in branches {
            writeln!(
                output,
                "Turn {} / branch {} (parent {:?}, {:?})\n{}",
                branch.turn,
                branch.branch_id,
                branch.parent_branch,
                branch.lifecycle,
                branch.prompt
            )
            .expect("String write");
            for (index, round) in branch.rounds.iter().enumerate() {
                writeln!(output, "  Round {}\n{}", index + 1, round.assistant)
                    .expect("String write");
                for call in &round.calls {
                    let result = crate::redact::truncate_utf8(call.result.clone(), self.max_bytes);
                    writeln!(
                        output,
                        "    Call {} {}\n      args: {}\n      ok={} refused={}\n      result: {}",
                        call.id,
                        call.name,
                        call.args,
                        call.ok,
                        call.refused,
                        result.replace('\n', "\n      ")
                    )
                    .expect("String write");
                }
            }
        }
        Ok(output)
    }
}
