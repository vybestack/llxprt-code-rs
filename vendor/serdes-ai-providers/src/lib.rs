//! OpenAI provider abstractions for serdes-ai.
//!
//! This retained release surface includes only the OpenAI implementation.
//!
//! ## Example
//!
//! ```rust,ignore
//! use serdes_ai_providers::{ProviderRegistry, OpenAIProvider};
//! use std::sync::Arc;
//!
//! // Create a registry
//! let registry = ProviderRegistry::new();
//!
//! // Register providers
//! registry.register(Arc::new(OpenAIProvider::new("sk-...")));
//!
//! // Infer provider from model string
//! let (provider, model) = registry.infer_provider("openai:gpt-4o")?;
//! ```
//!
//! ## Model Strings
//!
//! OpenAI models may use an `openai:` prefix or an inferred GPT/o-series name.

mod provider;
mod registry;

#[cfg(feature = "openai")]
mod openai;

pub use provider::*;
pub use registry::*;

#[cfg(feature = "openai")]
pub use openai::OpenAIProvider;

use std::sync::Arc;

/// Create a provider registry configured from environment variables.
///
/// This will check for API keys and create providers for each configured service.
pub fn from_env() -> ProviderRegistry {
    let registry = ProviderRegistry::new();

    #[cfg(feature = "openai")]
    if let Ok(provider) = OpenAIProvider::from_env() {
        registry.register(Arc::new(provider));
    }

    registry
}

/// Infer provider and model from a model string.
///
/// Supports formats like:
/// - `openai:gpt-4o` (explicit provider)
/// - `gpt-4o` (inferred from model name)
///
/// Returns a tuple of (provider, model_name).
pub fn infer(model_string: &str) -> Result<(BoxedProvider, String), ProviderError> {
    let registry = from_env();
    registry.infer_provider(model_string)
}

/// Prelude for common imports.
pub mod prelude {
    pub use crate::{
        from_env, global_registry, infer, BoxedProvider, Provider, ProviderConfig, ProviderError,
        ProviderRegistry,
    };

    #[cfg(feature = "openai")]
    pub use crate::OpenAIProvider;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_env_creates_registry() {
        let registry = from_env();
        // Registration depends on whether an OpenAI API key is present.
        let _ = registry.list();
    }

    #[test]
    fn test_global_registry() {
        let registry = global_registry();
        // Just verify global registry is accessible
        let _ = registry.list();
    }
}
