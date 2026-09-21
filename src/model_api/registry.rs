use crate::adapter::{make_adapter, ChatBackend};
use crate::model::ModelConfig;
use crate::profile::Profile;
use crate::settings::DEFAULT_REQUEST_TIMEOUT;
use serdes_ai_responses::client::OpenResponsesModel;

use super::anthropic_backend::AnthropicBackend;
use super::dependencies::{ConstructorKind, RuntimeDependencies};
use super::responses_backend::ResponsesBackend;

const CODEX_RESPONSES_BETA: &str = "responses=experimental";
const ORIGINATOR: &str = "llxprt-code";

pub(crate) struct ConstructedBackend {
    pub(crate) backend: Box<dyn ChatBackend>,
    pub(crate) secret_values: Vec<String>,
    pub(crate) context_limit: Option<u64>,
    pub(crate) max_rounds: usize,
}

pub(crate) fn construct_backend(
    profile: &Profile,
    session_id: &crate::session::SessionId,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
    model_params_mode: crate::settings::ModelParamsMode,
) -> Result<ConstructedBackend, String> {
    // The provider-layer seam: interpret the neutral parsed profile once, then
    // select the registration row for the resolved target.
    let resolved = crate::model_api::interpret::ResolvedProfile::interpret(profile)?;
    // The acceptance policy runs after the target resolves: `strict` and
    // `known-model` refuse unowned keys at load, before credentials are read;
    // `loose` (default) warns and the provider wire carries them verbatim.
    apply_model_params_policy(profile, &resolved, model_params_mode)?;
    let registration = dependencies
        .registrations()
        .iter()
        .find(|registration| registration.target == resolved.target)
        .ok_or_else(|| "selected model API is not registered".to_string())?;
    match registration.constructor {
        ConstructorKind::OpenAiChat => construct_chat(
            profile,
            dependencies,
            profile_from_file,
            allow_insecure_http,
        ),
        ConstructorKind::OpenAiResponses => construct_openai_responses(
            profile,
            &resolved,
            session_id,
            dependencies,
            profile_from_file,
            allow_insecure_http,
        ),
        ConstructorKind::AnthropicMessages => construct_anthropic(
            profile,
            &resolved,
            dependencies,
            profile_from_file,
            allow_insecure_http,
        ),
        ConstructorKind::CodexResponses => construct_codex(profile, &resolved, dependencies),
    }
}

/// The `modelParams` acceptance policy (issue 64).
///
/// `Loose` (default) forwards unrecognized keys on the provider wire and warns for
/// the keys this build knows but cannot serialize. `KnownModel` compares the
/// unrecognized keys against the checked-in model registry for the effective provider and
/// refuses a key the registry does not know. `Strict` refuses every unrecognized or
/// non-wire key at load.
fn apply_model_params_policy(
    profile: &Profile,
    resolved: &crate::model_api::interpret::ResolvedProfile,
    mode: crate::settings::ModelParamsMode,
) -> Result<(), String> {
    let forwarded: Vec<(String, &serde_json::Value)> = profile
        .model_params
        .forwarded
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    let unsupported: Vec<&String> = profile.model_params.unsupported.iter().collect();

    let refuse = |names: &[String], reason: &str| -> Result<(), String> {
        if names.is_empty() {
            return Ok(());
        }
        Err(format!(
            "{reason} unknown or unsupported modelParams key(s): {}",
            names.join(", ")
        ))
    };

    match mode {
        crate::settings::ModelParamsMode::Loose => {
            let names: Vec<String> = unsupported.iter().map(|n| n.to_string()).collect();
            for name in &names {
                // Unsupported keys cannot reach the wire; name them so the operator
                // knows they were not forwarded.
                crate::harness::eprint_status(&format!(
                    "warning: modelParams key `{name}` cannot be sent to the provider and was not forwarded"
                ));
            }
            let _ = forwarded;
            Ok(())
        }
        crate::settings::ModelParamsMode::KnownModel => {
            let unknown: Vec<String> = forwarded
                .iter()
                .filter(|(name, _)| {
                    !crate::model_api::model_registry::known_for(resolved.target.provider, name)
                })
                .map(|(name, _)| name.clone())
                .collect();
            refuse(&unknown, "known-model mode:")?;
            let names: Vec<String> = unsupported.iter().map(|n| n.to_string()).collect();
            for name in &names {
                crate::harness::eprint_status(&format!(
                    "warning: modelParams key `{name}` cannot be sent to the provider and was not forwarded"
                ));
            }
            Ok(())
        }
        crate::settings::ModelParamsMode::Strict => {
            let mut names: Vec<String> = forwarded
                .iter()
                .map(|(name, _)| name.clone())
                .chain(unsupported.iter().map(|n| n.to_string()))
                .collect();
            names.sort();
            refuse(&names, "strict mode:")?;
            Ok(())
        }
    }
}

/// The single request-timeout policy consumed at backend construction. The settings
/// resolver writes the resolved value onto the profile as provider data
/// (`ephemeral.timeout_ms`); this helper is the only timeout read in provider code and
/// falls back to the resolver's `DEFAULT_REQUEST_TIMEOUT` when no layer set one (kept
/// for direct construction paths such as tests).
fn resolved_timeout(profile: &Profile) -> std::time::Duration {
    std::time::Duration::from_millis(
        profile
            .ephemeral
            .timeout_ms
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT.as_millis() as u64),
    )
}

fn construct_chat(
    profile: &Profile,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let config = ModelConfig::from_profile_in(
        profile,
        profile_from_file,
        allow_insecure_http,
        dependencies.config_home().as_path(),
    )
        .map_err(|error| {
            if crate::model::insecure_http_error(&error) {
                "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
            } else {
                error.to_string()
            }
        })?;
    crate::limits::validate_timeout(config.timeout)?;
    let secret_values = config.secret_values();
    let context_limit = config.context_limit;
    let backend = make_adapter(&config).map_err(|error| error.to_string())?;
    let max_rounds = resolve_max_rounds(profile)?;
    Ok(ConstructedBackend {
        backend: Box::new(backend),
        secret_values,
        context_limit,
        max_rounds,
    })
}

/// `maxTurnsPerPrompt`: `-1` (and an absent knob, matching the TS ephemerals contract)
/// is unlimited — no round cap; the run still ends on the model's own completion, the
/// tool-call budget, the turn-time budget, and byte/output caps. A positive integer is
/// the cap.
fn resolve_max_rounds(profile: &Profile) -> Result<usize, String> {
    match profile.ephemeral.max_turns_per_prompt {
        None | Some(-1) => Ok(usize::MAX),
        Some(value) if value > 0 => {
            usize::try_from(value).map_err(|_| "resolved maximum turn count is invalid".to_string())
        }
        Some(_) => Err("resolved maximum turn count is invalid".to_string()),
    }
}

fn construct_openai_responses(
    profile: &Profile,
    resolved: &crate::model_api::interpret::ResolvedProfile,
    session_id: &crate::session::SessionId,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let endpoint = normalize_responses_endpoint(
        profile
            .ephemeral
            .base_url
            .as_ref()
            .map(crate::profile::RedactedUrl::full)
            .unwrap_or("https://api.openai.com/v1"),
    )?;
    crate::model::check_http_policy(endpoint.full(), allow_insecure_http).map_err(|error| {
        if crate::model::insecure_http_error(&error) {
            "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
        } else {
            error.to_string()
        }
    })?;
    let draft = resolved
        .openai_responses
        .as_ref()
        .ok_or_else(|| "OpenAI Responses settings were not resolved".to_string())?;

    // Credential policy after endpoint validation: a named provider key resolves
    // through the credential env selector then the secure store. The name is a
    // credential surface; the fixed value-free refusal is all that travels.
    let named_key = profile
        .ephemeral
        .auth_key_name
        .as_deref()
        .map(crate::provider_keys::resolve_named_key)
        .transpose()
        .map_err(|error| error.to_string())?;

    let api_key = match named_key {
        Some(key) => key,
        None => crate::model::resolve_api_key(
            profile,
            profile_from_file,
            dependencies.config_home().as_path(),
        )
        .map_err(|error| error.to_string())?,
    };
    let keyfile_path = profile.ephemeral.auth_keyfile_orig.clone();
    let secret_config = ModelConfig {
        model: profile.model.clone(),
        base_url: endpoint.clone(),
        api_key: api_key.clone(),
        keyfile_path,
        max_output_tokens: profile.ephemeral.max_output_tokens,
        timeout: Some(resolved_timeout(profile)),
        model_params: Some(profile.model_params.clone()),
        context_limit: profile.ephemeral.context_limit,
    };
    let secret_values = secret_config.secret_values();
    let model = serdes_ai::models::openai::OpenAIResponsesModel::new(&profile.model, api_key)
        .with_base_url(responses_transport_base(&endpoint))
        .with_settings(draft.finalize(session_id))
        .with_timeout(resolved_timeout(profile));
    let model_settings = serdes_ai::ModelSettings {
        max_tokens: profile.ephemeral.max_output_tokens,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        timeout: Some(resolved_timeout(profile)),
        ..Default::default()
    };
    crate::limits::validate_timeout(model_settings.timeout)?;
    let max_rounds = resolve_max_rounds(profile)?;
    Ok(ConstructedBackend {
        backend: Box::new(ResponsesBackend::new_openai(model, model_settings)),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn normalize_responses_endpoint(raw: &str) -> Result<crate::profile::RedactedUrl, String> {
    let mut url =
        url::Url::parse(raw).map_err(|_| "OpenAI Responses endpoint is invalid".to_string())?;
    let normalized_base = match url.path() {
        "" | "/" => "",
        "/v1" | "/v1/" => "/v1",
        "/responses" | "/responses/" => "",
        "/v1/responses" | "/v1/responses/" => "/v1",
        _ => return Err("OpenAI Responses endpoint has an unsupported route".to_string()),
    };
    url.set_path(normalized_base);
    crate::profile::RedactedUrl::parse(url.as_str())
        .map_err(|_| "OpenAI Responses endpoint is invalid".to_string())
}

fn responses_transport_base(endpoint: &crate::profile::RedactedUrl) -> &str {
    endpoint.full().trim_end_matches('/')
}

fn construct_anthropic(
    profile: &Profile,
    resolved: &crate::model_api::interpret::ResolvedProfile,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let base_url = profile
        .ephemeral
        .base_url
        .as_ref()
        .map(crate::profile::RedactedUrl::full)
        .unwrap_or("https://api.anthropic.com");
    crate::model::check_http_policy(base_url, allow_insecure_http).map_err(|error| {
        if crate::model::insecure_http_error(&error) {
            "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
        } else {
            error.to_string()
        }
    })?;
    // Credential policy after endpoint validation: a named provider key resolves
    // through the credential env selector then the secure store. The name is a
    // credential surface; the fixed value-free refusal is all that travels.
    let named_key = profile
        .ephemeral
        .auth_key_name
        .as_deref()
        .map(crate::provider_keys::resolve_named_key)
        .transpose()
        .map_err(|error| error.to_string())?;
    validate_anthropic_settings(profile)?;

    let api_key = match named_key {
        Some(key) => key,
        None => crate::model::resolve_api_key(
            profile,
            profile_from_file,
            dependencies.config_home().as_path(),
        )
        .map_err(|error| error.to_string())?,
    };
    let timeout = resolved_timeout(profile);
    let secret_config = ModelConfig {
        model: profile.model.clone(),
        base_url: crate::profile::RedactedUrl::parse(base_url)?,
        api_key: api_key.clone(),
        keyfile_path: profile.ephemeral.auth_keyfile_orig.clone(),
        max_output_tokens: profile.ephemeral.max_output_tokens,
        timeout: Some(timeout),
        model_params: Some(profile.model_params.clone()),
        context_limit: profile.ephemeral.context_limit,
    };
    let secret_values = secret_config.secret_values();
    let model_settings = anthropic_model_settings(profile, timeout);
    crate::limits::validate_timeout(model_settings.timeout)?;
    let mut model = serdes_ai::models::anthropic::AnthropicModel::new(&profile.model, api_key)
        .with_base_url(base_url.trim_end_matches('/'))
        .with_timeout(timeout);
    // Unrecognized `modelParams` keys travel as a flattened extra map on each
    // Messages request body (issue 64).
    model = model.with_extra(profile.model_params.forwarded.clone());
    let prompt_caching = resolved
        .anthropic
        .as_ref()
        .ok_or_else(|| "Anthropic Messages settings were not resolved".to_string())?
        .prompt_caching;
    let model = if prompt_caching == crate::model_api::settings::PromptCaching::Cached {
        model.with_caching()
    } else {
        model
    };
    let max_rounds = resolve_max_rounds(profile)?;

    Ok(ConstructedBackend {
        backend: Box::new(AnthropicBackend::new(model, model_settings)),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn anthropic_model_settings(
    profile: &Profile,
    timeout: std::time::Duration,
) -> serdes_ai::ModelSettings {
    serdes_ai::ModelSettings {
        max_tokens: profile.ephemeral.max_output_tokens,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        top_k: profile.model_params.top_k,
        timeout: Some(timeout),
        ..Default::default()
    }
}

fn validate_anthropic_settings(profile: &Profile) -> Result<(), String> {
    let unsupported = profile
        .ephemeral
        .unsupported
        .iter()
        .chain(profile.model_params.unsupported.iter())
        .cloned()
        .collect::<Vec<_>>();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unsupported Anthropic Messages setting(s): {}",
            unsupported.join(", ")
        ))
    }
}

fn construct_codex(
    profile: &Profile,
    resolved: &crate::model_api::interpret::ResolvedProfile,
    dependencies: &RuntimeDependencies,
) -> Result<ConstructedBackend, String> {
    let draft = resolved
        .codex
        .as_ref()
        .ok_or_else(|| "Codex Responses settings were not resolved".to_string())?;
    let credential = dependencies
        .credential_source()
        .load(dependencies.clock())
        .map_err(|error| error.to_string())?;
    let secret_values = credential
        .secret_values()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();

    let user_agent = format!("llxprt-code-rs/{}", env!("CARGO_PKG_VERSION"));
    let mut model = OpenResponsesModel::new(draft.model(), draft.endpoint().responses_url())
        .codex_http()
        .bearer(credential.access_token())
        .header("chatgpt-account-id", credential.account_id())
        .header("OpenAI-Beta", CODEX_RESPONSES_BETA)
        .header("originator", ORIGINATOR)
        .header("User-Agent", user_agent);
    if let Some(reasoning) = draft.responses_reasoning() {
        model = model.with_reasoning(reasoning);
    }

    let max_rounds = resolve_max_rounds(profile)?;
    // Mirror the OpenAI Responses path: the profile's output-token bound and sampling
    // parameters travel in `ModelSettings`; the vendored Codex client honors none of
    // them on its own.
    let model_settings = codex_model_settings(profile);
    crate::limits::validate_timeout(model_settings.timeout)?;
    Ok(ConstructedBackend {
        backend: Box::new(ResponsesBackend::new(model, model_settings)),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn codex_model_settings(profile: &Profile) -> serdes_ai::ModelSettings {
    serdes_ai::ModelSettings {
        // The ChatGPT codex backend rejects `max_output_tokens` outright, so
        // the output bound stays host-side (context limit + turn budget).
        max_tokens: None,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        timeout: Some(resolved_timeout(profile)),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
