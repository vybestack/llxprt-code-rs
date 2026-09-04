//! Runner-neutral scenario manifests for the Phase 0 context-management evals (#37).
//!
//! A manifest describes stimuli, profile budget, scripted provider behaviour, fixture
//! pressure, expected terminal evidence, and the phase that owns turning it green. It
//! never contains runner argv: adapters translate a [`Scenario`] into runner-specific
//! commands, so the same manifest drives the Rust acceptance target and the TypeScript
//! reference runner.

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::context_eval::faults;

/// Current manifest schema version. Bumping it is a breaking eval change.
pub const SCHEMA_VERSION: u32 = 1;
/// Highest tool-round count a manifest may request (keeps a red run bounded).
pub const MAX_TOOL_ROUNDS: u32 = 16;
/// Largest deterministic fixture expansion per round (bytes).
pub const MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// Comparison arm a scenario belongs to.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Arm {
    /// Feature acceptance scenario; stays red until its owning phase lands.
    Feature,
    /// Status-quo full-replay baseline arm.
    StatusQuo,
    /// Minimum-management-floor configuration: arm-specific runtime config carried into
    /// the generated profile, so arm selection changes actual runtime behavior.
    MinimumFloor,
}

impl Arm {
    /// Stable kebab-case name used in reports and records.
    pub fn name(self) -> &'static str {
        match self {
            Arm::Feature => "feature",
            Arm::StatusQuo => "status-quo",
            Arm::MinimumFloor => "minimum-floor",
        }
    }
}

/// Expected status of a scenario in the current phase.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedStatus {
    Red,
    Green,
}

impl ExpectedStatus {
    /// Stable lowercase name for manifests, reports, and records.
    pub fn name(self) -> &'static str {
        match self {
            ExpectedStatus::Red => "red",
            ExpectedStatus::Green => "green",
        }
    }
}

/// Profile budget the adapter materialises for the run. No credentials live here.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSpec {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub context_limit_tokens: u64,
    pub max_output_tokens: u64,
}

/// Stimulus: the opening prompt plus optional continuation prompts.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stimulus {
    pub prompt: String,
    #[serde(default)]
    pub followups: Vec<String>,
}

/// Deterministic pressure the fixtures apply before the wall.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WallSpec {
    #[serde(default)]
    pub tool_rounds: u32,
    #[serde(default)]
    pub tool_output_bytes: usize,
    #[serde(default)]
    pub fixture: String,
}

/// Independent grader assertions (never model-authored claims).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertions {
    #[serde(default)]
    pub required_final_marker: Option<String>,
    #[serde(default)]
    pub required_answer_tokens: Vec<String>,
    #[serde(default)]
    pub required_context_artifacts: Vec<String>,
    #[serde(default)]
    pub required_outcomes: Vec<String>,
}

/// Declared fault injection points.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faults {
    #[serde(default)]
    pub injected: Vec<String>,
}

/// Arm-specific runtime configuration materialised into the generated profile.
///
/// Two arms may describe the same stimulus and still be a comparison only if the
/// configuration they install selects different runtime behavior. `context_limit`
/// is the knob the acceptance target already honors, so the status-quo arm keeps the
/// profile limit and the minimum-floor arm overrides it to the arm's floor.
#[derive(Clone, Debug, Deserialize)]
pub struct RuntimeConfig {
    /// Effective context limit (tokens) this arm installs.
    pub context_limit: u64,
    /// Names the installed configuration in reports so arm runs are distinguishable.
    pub name: String,
}

/// One runner-neutral scenario.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u32,
    pub id: String,
    /// Phase that may turn this scenario green.
    pub owner_phase: u8,
    pub arm: Arm,
    pub expected_status: ExpectedStatus,
    pub expected_reason_class: String,
    /// Broad red scenarios accept any clean non-pass failure reason.
    #[serde(default)]
    pub accept_any_reason: bool,
    pub profile: ProfileSpec,
    pub stimulus: Stimulus,
    #[serde(default)]
    pub wall: WallSpec,
    #[serde(default)]
    pub assertions: Assertions,
    #[serde(default)]
    pub faults: Faults,
    /// Arm-specific runtime configuration. Required: without it two arms differ only
    /// in label, which is exactly the non-selecting comparison the program forbids.
    pub runtime: RuntimeConfig,
}

impl Scenario {
    /// Validate a parsed scenario against the schema contract.
    pub fn validate(&self, fixtures: &Path) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "scenario {id} has schema_version {v}, expected {SCHEMA_VERSION}",
                id = self.id,
                v = self.schema_version
            ));
        }
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "scenario id {id:?} must be lowercase kebab-case",
                id = self.id
            ));
        }
        if self.owner_phase == 0 || self.owner_phase > 9 {
            return Err(format!(
                "scenario {id} owner_phase {phase} out of range",
                id = self.id,
                phase = self.owner_phase
            ));
        }
        if self.expected_reason_class.trim().is_empty() {
            return Err(format!(
                "scenario {id} has an empty expected_reason_class",
                id = self.id
            ));
        }
        if self.profile.name.trim().is_empty() || self.profile.context_limit_tokens == 0 {
            return Err(format!(
                "scenario {id} has an unusable profile",
                id = self.id
            ));
        }
        if self.stimulus.prompt.trim().is_empty() {
            return Err(format!("scenario {id} has an empty prompt", id = self.id));
        }
        self.validate_wall(fixtures)?;
        if self.runtime.name.trim().is_empty() || self.runtime.context_limit == 0 {
            return Err(format!(
                "scenario {id:?} has an unusable runtime config",
                id = self.id
            ));
        }
        faults::validate(&self.faults.injected)
            .map_err(|e| format!("scenario {id}: {e}", id = self.id))?;
        if self.arm == Arm::MinimumFloor && self.expected_status == ExpectedStatus::Green {
            return Err(format!(
                "scenario {id:?}: the minimum-floor arm cannot declare green before its \
                 comparison against the status-quo arm exists",
                id = self.id
            ));
        }
        Ok(())
    }

    /// Prompts in drive order: the opening prompt then every followup.
    pub fn prompts(&self) -> Vec<&str> {
        let mut all = vec![self.stimulus.prompt.as_str()];
        all.extend(self.stimulus.followups.iter().map(String::as_str));
        all
    }
    /// Wall and fixture bounds (delegated so the contract stays readable): round count,
    /// per-round byte pressure, and that a round-requesting scenario names a real,
    /// bounded fixture file.
    fn validate_wall(&self, fixtures: &Path) -> Result<(), String> {
        if self.wall.tool_rounds > MAX_TOOL_ROUNDS {
            return Err(format!(
                "scenario {id} asks for {rounds} tool rounds (max {MAX_TOOL_ROUNDS})",
                id = self.id,
                rounds = self.wall.tool_rounds
            ));
        }
        if self.wall.tool_output_bytes > MAX_BLOCK_BYTES {
            return Err(format!(
                "scenario {id} asks for {bytes} bytes per round (max {MAX_BLOCK_BYTES})",
                id = self.id,
                bytes = self.wall.tool_output_bytes
            ));
        }
        if self.wall.tool_rounds > 0 {
            if self.wall.fixture.trim().is_empty() {
                return Err(format!(
                    "scenario {id} sets tool_rounds without a fixture",
                    id = self.id
                ));
            }
            let path = fixtures.join(&self.wall.fixture);
            let meta = fs::metadata(&path)
                .map_err(|e| format!("scenario {} fixture {}: {e}", self.id, path.display()))?;
            if !meta.is_file() || meta.len() > 256 * 1024 {
                return Err(format!(
                    "scenario {} fixture {} must be a file of at most 256 KiB",
                    self.id,
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

/// Load every `*.toml` manifest under `root`, sorted by path for determinism.
pub fn load_dir(root: &Path, fixtures: &Path) -> Result<Vec<(PathBuf, Scenario)>, String> {
    let entries = fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let text =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let scen: Scenario = toml::from_str(&text)
            .map_err(|e| format!("manifest {} rejected by schema: {e}", path.display()))?;
        scen.validate(fixtures)?;
        out.push((path, scen));
    }
    if out.is_empty() {
        return Err(format!("no scenario manifests under {}", root.display()));
    }
    Ok(out)
}

/// Parse one manifest from text (used by the schema-rejection self-tests).
pub fn parse_str(text: &str, fixtures: &Path) -> Result<Scenario, String> {
    let scen: Scenario =
        toml::from_str(text).map_err(|e| format!("manifest rejected by schema: {e}"))?;
    scen.validate(fixtures)?;
    Ok(scen)
}
