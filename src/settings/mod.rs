//! Typed layered configuration and its provenance.

mod resolve;
mod schema;
mod user_file;

pub use resolve::{resolve, SettingsError};
pub use schema::SETTINGS_SCHEMA_ID;
pub use user_file::load_user_file;

use std::path::PathBuf;
use std::time::Duration;

/// The single provider request-timeout policy default (900s), preserving today's
/// effective value everywhere a provider previously hardcoded 900s. Providers consume
/// the resolved value; this constant is the sole timeout default in the codebase.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    UserFile,
    Profile,
    Env,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Settings {
    pub provider: ResolvedProvider,
    pub budgets: ResolvedBudgets,
    pub paths: ResolvedPaths,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedProvider {
    pub base_url: Resolved<String>,
    pub model: Resolved<String>,
    pub profile_path: Resolved<PathBuf>,
    pub model_params_mode: Resolved<ModelParamsMode>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedBudgets {
    pub max_tool_calls: Resolved<i64>,
    pub max_shell_output: Resolved<u64>,
    pub max_tool_output: Resolved<u64>,
    pub max_turn_output: Resolved<u64>,
    /// Digest admission floor (issue 125): results at or above this size are bulk
    /// evidence and are digested before they reach the request list. Defaults to the
    /// version-1 rule table's baseline floor.
    pub digest_size_floor: Resolved<u64>,
    #[serde(with = "duration_option")]
    pub turn_time: Resolved<Option<Duration>>,
    #[serde(with = "duration")]
    pub request_timeout: Resolved<Duration>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedPaths {
    pub config_root: Resolved<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SettingsLayer {
    #[serde(skip_serializing_if = "SettingsProvider::is_empty")]
    pub provider: SettingsProvider,
    #[serde(skip_serializing_if = "SettingsBudgets::is_empty")]
    pub budgets: SettingsBudgets,
    #[serde(skip_serializing_if = "SettingsPaths::is_empty")]
    pub paths: SettingsPaths,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsProvider {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_path: Option<PathBuf>,
    /// The `modelParams` acceptance policy: `loose` (default), `known-model`, or
    /// `strict`. Stored as the on-disk spelling; resolved into [`ModelParamsMode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_params_mode: Option<String>,
}

impl SettingsProvider {
    fn is_empty(&self) -> bool {
        self.base_url.is_none()
            && self.model.is_none()
            && self.profile_path.is_none()
            && self.model_params_mode.is_none()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_time: Option<String>,
    #[serde(
        rename = "max-shell-output",
        alias = "max_shell_output",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_shell_output: Option<u64>,
    #[serde(
        rename = "max-tool-output",
        alias = "max_tool_output",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tool_output: Option<u64>,
    #[serde(
        rename = "max-turn-output",
        alias = "max_turn_output",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_turn_output: Option<u64>,
    #[serde(
        rename = "digest-size-floor",
        alias = "digest_size_floor",
        skip_serializing_if = "Option::is_none"
    )]
    pub digest_size_floor: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout: Option<String>,
}

impl SettingsBudgets {
    fn is_empty(&self) -> bool {
        self.max_tool_calls.is_none()
            && self.turn_time.is_none()
            && self.max_shell_output.is_none()
            && self.max_tool_output.is_none()
            && self.max_turn_output.is_none()
            && self.digest_size_floor.is_none()
            && self.request_timeout.is_none()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPaths {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_root: Option<PathBuf>,
}

impl SettingsPaths {
    fn is_empty(&self) -> bool {
        self.config_root.is_none()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SettingsLayers {
    pub user_file: SettingsLayer,
    pub profile: SettingsLayer,
    pub env: SettingsLayer,
    pub cli: SettingsLayer,
    pub config_root: PathBuf,
}

/// Read the process environment settings layer. This is the sole settings-env reader.
pub fn environment_layer() -> Result<SettingsLayer, String> {
    let model_params_mode = std::env::var("LLXPRT_MODEL_PARAMS_MODE")
        .ok()
        .map(|value| {
            ModelParamsMode::parse(&value)
                .map(|mode| mode.as_str().to_string())
                .map_err(|_| {
                    format!(
                    "LLXPRT_MODEL_PARAMS_MODE must be loose, known-model, or strict (got {value})"
                )
                })
        })
        .transpose()?;
    Ok(SettingsLayer {
        provider: SettingsProvider {
            base_url: std::env::var("LLXPRT_BASE_URL").ok(),
            model_params_mode,
            ..Default::default()
        },
        budgets: SettingsBudgets {
            max_tool_calls: std::env::var("LLXPRT_MAX_TOOL_CALLS")
                .ok()
                .map(|value| {
                    value.parse().map_err(|_| {
                        "--max-tool-calls must be -1 or an integer from 1 through 512".to_string()
                    })
                })
                .transpose()?,
            turn_time: std::env::var("LLXPRT_TURN_TIME").ok(),
            max_shell_output: env_byte_cap("LLXPRT_MAX_SHELL_OUTPUT", "--max-shell-output")?,
            max_tool_output: env_byte_cap("LLXPRT_MAX_TOOL_OUTPUT", "--max-tool-output")?,
            max_turn_output: env_byte_cap("LLXPRT_MAX_TURN_OUTPUT", "--max-turn-output")?,
            digest_size_floor: env_byte_cap("LLXPRT_DIGEST_SIZE_FLOOR", "--digest-size-floor")?,
            request_timeout: std::env::var("LLXPRT_REQUEST_TIMEOUT").ok(),
        },
        ..Default::default()
    })
}

/// Read one byte-cap setting from the environment, validating it like the CLI flag.
fn env_byte_cap(var: &str, flag: &str) -> Result<Option<u64>, String> {
    std::env::var(var)
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{flag} must be an integer byte count (got {value})"))
        })
        .transpose()
}

/// Validate the resolved output caps (issue 77): a per-result cap above the aggregate
/// turn cap is a typed configuration error naming both values, never a silent clamp.
pub fn validate_output_caps(shell: u64, tool: u64, turn: u64) -> Result<(), String> {
    for (flag, value) in [("--max-shell-output", shell), ("--max-tool-output", tool)] {
        if value > turn {
            return Err(format!(
                "{flag} ({value}) exceeds --max-turn-output ({turn})"
            ));
        }
    }
    Ok(())
}

/// Validate the resolved digest admission floor (issue 125). The floor rides into the
/// versioned filter registry as a relaxation, and the registry refuses a tightening, so
/// a floor below the baseline is a typed configuration error instead of a silent change
/// to what an already-digested record means.
pub fn validate_digest_size_floor(value: u64) -> Result<u64, String> {
    let baseline = baseline_floor_u64();
    if value < baseline {
        return Err(format!(
            "--digest-size-floor ({value}) must be at least the baseline floor ({baseline})"
        ));
    }
    Ok(value)
}

/// The version-1 baseline digest admission floor as `u64`, shared by validation and
/// resolution so the conversion lives in one place. Fails closed: the baseline is a
/// `usize` constant that always fits, so a target with a `usize` wider than `u64` is an
/// unsupported-target invariant violation worth failing on rather than rewriting the
/// baseline to `u64::MAX` (which would silently admit every floor).
pub(crate) fn baseline_floor_u64() -> u64 {
    u64::try_from(crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR)
        .expect("the baseline digest-size floor always fits u64")
}

/// The `modelParams` acceptance policy (issue 64).
///
/// `loose` is the default: unknown `modelParams` keys are forwarded verbatim on the
/// provider wire and the provider's own error is the validation. `known-model` checks
/// the profile against the checked-in model registry at load. `strict` refuses unknown
/// keys at load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ModelParamsMode {
    #[default]
    Loose,
    KnownModel,
    Strict,
}

impl ModelParamsMode {
    /// Parse the on-disk spelling used by the settings file, `LLXPRT_MODEL_PARAMS_MODE`,
    /// and `--model-params-mode`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "loose" => Ok(Self::Loose),
            "known-model" => Ok(Self::KnownModel),
            "strict" => Ok(Self::Strict),
            other => Err(format!(
                "--model-params-mode must be loose, known-model, or strict (got {other})"
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loose => "loose",
            Self::KnownModel => "known-model",
            Self::Strict => "strict",
        }
    }
}

/// Validate the max-tool-call setting used by both CLI-equivalent layers.
pub fn validate_max_tool_calls(value: i64) -> Result<i64, String> {
    if value == -1 || (1..=512).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "--max-tool-calls must be -1 or an integer from 1 through 512 (got {value})"
        ))
    }
}
/// Parse a turn-time value using the CLI grammar.
pub fn parse_turn_time(raw: &str) -> Result<Option<Duration>, String> {
    let raw = raw.trim();
    let (digits, unit) = match raw.char_indices().rfind(|(_, c)| !c.is_ascii_digit()) {
        Some((idx, c)) => (&raw[..idx], c),
        None => (raw, '\0'),
    };
    let seconds_per_unit = match unit {
        '\0' if digits == "0" => return Ok(None),
        '\0' => {
            return Err(format!(
                "--turn-time needs an s/m/h unit (got {raw:?}); pass 0 to disable"
            ))
        }
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(format!("--turn-time unit must be s, m, or h (got {raw})")),
    };
    let count = digits
        .parse::<u64>()
        .map_err(|_| format!("--turn-time needs an integer count (got {raw})"))?;
    let seconds = count
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| format!("--turn-time {raw} overflows"))?;
    Ok((seconds != 0).then(|| Duration::from_secs(seconds)))
}

/// Parse a request-timeout value using the CLI grammar: seconds as a bare integer, or
/// a duration string with an `s`, `m`, `h`, or `ms` unit (the `ms` unit is how
/// the profile layer expresses its provider `timeoutMs` data).
pub fn parse_request_timeout(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    let (digits, unit) = if let Some(stripped) = raw.strip_suffix("ms") {
        (stripped, "ms")
    } else if let Some(stripped) = raw.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = raw.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = raw.strip_suffix('h') {
        (stripped, "h")
    } else {
        (raw, "")
    };
    let milliseconds_per_unit = match unit {
        "ms" => 1,
        "s" | "" => 1000,
        "m" => 60 * 1000,
        "h" => 3600 * 1000,
        _ => {
            return Err(format!(
                "--request-timeout unit must be s, m, h, or ms (got {raw})"
            ))
        }
    };
    let count = digits
        .parse::<u64>()
        .map_err(|_| format!("--request-timeout needs an integer count (got {raw})"))?;
    let ms = count
        .checked_mul(milliseconds_per_unit)
        .ok_or_else(|| format!("--request-timeout {raw} overflows"))?;
    Ok(Duration::from_millis(ms))
}

mod duration {
    use serde::{Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(
        value: &crate::settings::Resolved<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            value: u64,
            source: &'a super::Source,
        }
        Wire {
            value: value.value.as_secs(),
            source: &value.source,
        }
        .serialize(serializer)
    }
}

mod duration_option {
    use serde::{Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(
        value: &crate::settings::Resolved<Option<Duration>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            value: Option<u64>,
            source: &'a super::Source,
        }
        Wire {
            value: value.value.map(|d| d.as_secs()),
            source: &value.source,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests;
