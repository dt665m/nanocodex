//! Provider recipe for the shared Nanocodex harness.
use nanocodex_oai_api::{Model, OpenAi, OpenAiBuilder, OpenAiError, Thinking, auth::OpenAiAuth};

use crate::{BuilderBackend, NanocodexBuilder};

/// Muse Spark 1.3 using HTTP/SSE Responses and client-owned conversation history.
#[derive(Clone)]
pub struct Muse {
    inner: OpenAi,
}

/// Validates a Muse provider recipe before starting the agent driver.
pub struct MuseBuilder {
    inner: OpenAiBuilder,
    model: Model,
}

impl Muse {
    /// Creates a recipe with Meta API-key authentication. The key is never logged.
    #[must_use]
    pub fn builder(auth: impl Into<OpenAiAuth>) -> MuseBuilder {
        MuseBuilder {
            model: Model::Muse,
            inner: OpenAi::builder(auth.into())
                .model(Model::Muse)
                .api_base_url("https://api.meta.ai/v1")
                .context_window_tokens(Model::Muse.max_context_window_tokens())
                .raw_api_events(false),
        }
    }
}

impl MuseBuilder {
    /// Selects Standard or Contributor by the shared model identifier.
    /// Contributor permits Meta to train on prompts and completions.
    #[must_use]
    pub fn model(mut self, model: Model) -> Self {
        self.model = model;
        self.inner = self.inner.model(model);
        self
    }

    /// Overrides the Responses base URL, for a gateway or local protocol testing.
    #[must_use]
    pub fn api_base_url(mut self, url: impl Into<String>) -> Self {
        self.inner = self.inner.api_base_url(url);
        self
    }

    /// Selects the reasoning effort (defaults to low).
    #[must_use]
    pub fn thinking(mut self, thinking: Thinking) -> Self {
        self.inner = self.inner.thinking(thinking);
        self
    }

    /// Uses a smaller context budget for automatic compaction.
    #[must_use]
    pub fn context_window_tokens(mut self, tokens: u64) -> Self {
        self.inner = self.inner.context_window_tokens(tokens);
        self
    }

    /// Validates the provider configuration without making a network request.
    ///
    /// # Errors
    /// Returns an error for an empty key, endpoint, or context budget.
    pub fn build(self) -> Result<Muse, OpenAiError> {
        if !self.model.is_muse() {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "Muse requires muse-spark-1.3 or muse-spark-1.3-contributor",
            });
        }
        let inner = self.inner.build()?;
        Ok(Muse { inner })
    }
}

impl BuilderBackend for Muse {
    type Builder = NanocodexBuilder;

    fn into_builder(self) -> Self::Builder {
        self.inner.into_builder()
    }
}
