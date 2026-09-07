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
        model_params_mode: pick_model_params_mode(&layers)?,
    };
    let max = pick(16, &layers, |x| &x.budgets.max_tool_calls);
    validate_max_tool_calls(max.value).map_err(SettingsError::Invalid)?;
    let max_shell_output = pick(
        u64::try_from(crate::tools::output_limits::MAX_SHELL_OUTPUT_DEFAULT).unwrap_or(u64::MAX),
        &layers,
        |x| &x.budgets.max_shell_output,
    );
    let max_tool_output = pick(
        u64::try_from(crate::tools::output_limits::MAX_TOOL_OUTPUT_DEFAULT).unwrap_or(u64::MAX),
        &layers,
        |x| &x.budgets.max_tool_output,
    );
    let max_turn_output = pick(
        u64::try_from(crate::limits::MAX_TURN_OUTPUT_BYTES).unwrap_or(u64::MAX),
        &layers,
        |x| &x.budgets.max_turn_output,
    );
    validate_output_caps(
        max_shell_output.value,
        max_tool_output.value,
        max_turn_output.value,
    )
    .map_err(SettingsError::Invalid)?;
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
            max_shell_output,
            max_tool_output,
            max_turn_output,
            turn_time,
        },
        paths: ResolvedPaths { config_root },
    })
}

/// Resolve the `modelParams` acceptance policy across the layers. The raw spelling is
/// validated here so an invalid value fails resolution regardless of which layer set it.
fn pick_model_params_mode(
    layers: &SettingsLayers,
) -> Result<Resolved<ModelParamsMode>, SettingsError> {
    let winner = [
        (Source::Cli, &layers.cli),
        (Source::Env, &layers.env),
        (Source::Profile, &layers.profile),
        (Source::UserFile, &layers.user_file),
    ]
    .into_iter()
    .find_map(|(source, layer)| {
        layer
            .provider
            .model_params_mode
            .as_deref()
            .map(|raw| (source, raw))
    });
    Ok(match winner {
        Some((source, raw)) => Resolved {
            value: ModelParamsMode::parse(raw).map_err(SettingsError::Invalid)?,
            source,
        },
        None => Resolved {
            value: ModelParamsMode::default(),
            source: Source::Default,
        },
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
