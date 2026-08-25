use llxprt_code_rs::adapter::ModelErrorAdapter;
use llxprt_code_rs::agent::AgentError;
use llxprt_code_rs::cli::{AppError, Code};
use llxprt_code_rs::session::StoreError;

const SECRET: &str = "sk-error-contract-secret";

fn adversarial_message() -> String {
    format!(
        "Authorization: Bearer {SECRET} https://user:{SECRET}@example.invalid/private?api_key={SECRET}#fragment {}",
        "x".repeat(llxprt_code_rs::redact::MAX_DIAGNOSTIC_BYTES * 2)
    )
}

fn assert_contract<T: std::error::Error>() {}

fn assert_safe(error: &(dyn std::error::Error + 'static)) {
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(rendered.len() <= llxprt_code_rs::redact::MAX_DIAGNOSTIC_BYTES);
        assert!(!rendered.contains(SECRET));
        assert!(!rendered.contains("/private"));
        assert!(!rendered.contains("#fragment"));
    }
    assert!(error.source().is_none());
}

#[test]
fn public_errors_implement_bounded_secret_safe_standard_contracts() {
    assert_contract::<AppError>();
    assert_contract::<AgentError>();
    assert_contract::<ModelErrorAdapter>();
    assert_contract::<StoreError>();

    assert_safe(&AppError::new(
        Code::Config,
        "config",
        adversarial_message(),
    ));
    assert_safe(&AgentError::new(
        Code::Model,
        "model",
        adversarial_message(),
    ));
    assert_safe(&ModelErrorAdapter {
        key: "adapter",
        message: adversarial_message(),
        code: Code::Model,
    });
    assert_safe(&StoreError::Corrupt(adversarial_message()));
}

#[test]
fn generic_model_error_and_public_wrappers_hide_provider_text() {
    let model = serdes_ai::models::ModelError::http(500, SECRET);
    for rendered in [model.to_string(), format!("{model:?}")] {
        assert!(!rendered.contains(SECRET));
    }
    assert!(std::error::Error::source(&model).is_none());

    let agent = serdes_ai::agent::errors::AgentRunError::from(serdes_ai::models::ModelError::http(
        500, SECRET,
    ));
    let direct =
        serdes_ai::direct::DirectError::from(serdes_ai::models::ModelError::http(500, SECRET));
    for rendered in [
        agent.to_string(),
        format!("{agent:?}"),
        direct.to_string(),
        format!("{direct:?}"),
    ] {
        assert!(!rendered.contains(SECRET));
    }
}
