//! Typed layered configuration and its provenance.

mod resolve;
mod schema;
mod user_file;

pub use resolve::{resolve, SettingsError};
pub use schema::SETTINGS_SCHEMA_ID;
pub use user_file::load_user_file;

use std::path::PathBuf;
use std::time::Duration;

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
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedBudgets {
    pub max_tool_calls: Resolved<i64>,
    #[serde(with = "duration_option")]
    pub turn_time: Resolved<Option<Duration>>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedPaths {
    pub config_root: Resolved<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SettingsLayer {
    pub provider: SettingsProvider,
    pub budgets: SettingsBudgets,
    pub paths: SettingsPaths,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsProvider {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub profile_path: Option<PathBuf>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsBudgets {
    pub max_tool_calls: Option<i64>,
    pub turn_time: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPaths {
    pub config_root: Option<PathBuf>,
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
    Ok(SettingsLayer {
        provider: SettingsProvider {
            base_url: std::env::var("LLXPRT_BASE_URL").ok(),
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
        },
        ..Default::default()
    })
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
                "--turn-time needs an s/m/h unit (got {raw}); pass 0 to disable"
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
