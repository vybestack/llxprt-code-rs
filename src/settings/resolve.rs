use super::*;
use crate::config::std_profile_dir;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("configuration root: {0}")]
    ConfigRoot(String),
}

pub fn resolve(mut layers: SettingsLayers) -> Result<Settings, SettingsError> {
    if layers.config_root.as_os_str().is_empty() {
        layers.config_root = std_profile_dir().map_err(SettingsError::ConfigRoot)?;
    }
    let config_root = pick(layers.config_root.clone(), &layers, |x| {
        &x.paths.config_root
    });
    let provider = ResolvedProvider {
        base_url: pick("https://api.openai.com/v1".to_string(), &layers, |x| {
            &x.provider.base_url
        }),
        model: pick("gpt-4o".to_string(), &layers, |x| &x.provider.model),
        profile_path: pick(
            config_root.value.join("profiles/dsflash-mi300x.json"),
            &layers,
            |x| &x.provider.profile_path,
        ),
    };
    let max = pick(16, &layers, |x| &x.budgets.max_tool_calls);
    validate_max_tool_calls(max.value).map_err(SettingsError::Invalid)?;
    let raw_time = pick_optional(&layers, |x| &x.budgets.turn_time);
    let turn_time = Resolved {
        value: raw_time
            .value
            .as_deref()
            .map(parse_turn_time)
            .transpose()
            .map_err(SettingsError::Invalid)?
            .flatten(),
        source: raw_time.source,
    };
    Ok(Settings {
        provider,
        budgets: ResolvedBudgets {
            max_tool_calls: max,
            turn_time,
        },
        paths: ResolvedPaths { config_root },
    })
}
fn pick<T: Clone>(
    default: T,
    layers: &SettingsLayers,
    get: impl Fn(&SettingsLayer) -> &Option<T>,
) -> Resolved<T> {
    [
        (Source::Cli, &layers.cli),
        (Source::Env, &layers.env),
        (Source::Profile, &layers.profile),
        (Source::UserFile, &layers.user_file),
    ]
    .into_iter()
    .find_map(|(source, layer)| get(layer).clone().map(|value| Resolved { value, source }))
    .unwrap_or(Resolved {
        value: default,
        source: Source::Default,
    })
}

fn pick_optional<T: Clone>(
    layers: &SettingsLayers,
    get: impl Fn(&SettingsLayer) -> &Option<T>,
) -> Resolved<Option<T>> {
    [
        (Source::Cli, &layers.cli),
        (Source::Env, &layers.env),
        (Source::Profile, &layers.profile),
        (Source::UserFile, &layers.user_file),
    ]
    .into_iter()
    .find_map(|(source, layer)| {
        get(layer).clone().map(|value| Resolved {
            value: Some(value),
            source,
        })
    })
    .unwrap_or(Resolved {
        value: None,
        source: Source::Default,
    })
}
