//! Provider recipe for the Muse harness.
use nanocodex_oai_api::{Model, OpenAi, OpenAiBuilder, OpenAiError, Thinking, auth::OpenAiAuth};

use crate::{BuilderBackend, NanocodexBuilder};

const MUSE_SYSTEM_PROMPT: &str = include_str!("../prompts/muse.md");

/// Muse using HTTP/SSE Responses and client-owned conversation history.
#[derive(Clone)]
pub struct Muse {
    inner: OpenAi<crate::service::MuseServiceFactory>,
    model: Model,
}

/// Validates a Muse provider recipe before starting the agent driver.
pub struct MuseBuilder {
    inner: OpenAiBuilder<crate::service::MuseServiceFactory>,
    model: Model,
}

impl Muse {
    /// Shared model identifier configured on this provider.
    #[must_use]
    pub const fn selected_model(&self) -> Model {
        self.model
    }

    /// Creates a recipe with Meta API-key authentication. The key is never logged.
    #[must_use]
    pub fn builder(auth: impl Into<OpenAiAuth>) -> MuseBuilder {
        MuseBuilder {
            model: crate::MuseModel::Spark.into(),
            inner: OpenAi::builder(auth.into())
                .model(crate::MuseModel::Spark.into())
                .api_base_url("https://api.meta.ai/v1")
                .context_window_tokens(Model::MuseSpark13.max_context_window_tokens())
                .transport(nanocodex_oai_api::transport::ResponsesTransport::Https)
                .store(false)
                .history(nanocodex_oai_api::transport::ResponsesHistory::FullReplay)
                .service_factory(crate::service::MuseServiceFactory)
                .raw_api_events(false),
        }
    }
}

impl MuseBuilder {
    /// Selects Spark, Contributor, Wonder or Wonder Full by the shared identifier.
    /// Contributor permits Meta to train on prompts and completions.
    #[must_use]
    pub fn model(mut self, model: impl Into<Model>) -> Self {
        let model = model.into();
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

    /// Installs the embedding-owned HTTP and timer implementation on WebAssembly.
    /// The host must honor the request's Responses Lite and additional HTTP headers.
    #[cfg(any(target_family = "wasm", docsrs))]
    #[cfg_attr(docsrs, doc(cfg(target_family = "wasm")))]
    #[must_use]
    pub fn host_transport(
        mut self,
        transport: impl nanocodex_oai_api::transport::host::HostTransport,
    ) -> Self {
        self.inner = self.inner.host_transport(transport);
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
        if crate::MuseModel::try_from(self.model).is_err() {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "Muse requires muse-spark-1.3, muse-spark-1.3-contributor, muse-wonder-1.0, or muse-wonder-full-1.0",
            });
        }
        let inner = self.inner.build()?;
        Ok(Muse {
            inner,
            model: self.model,
        })
    }
}

impl BuilderBackend for Muse {
    type Builder = NanocodexBuilder<crate::service::MuseServiceFactory>;

    fn into_builder(self) -> Self::Builder {
        let prompt = system_prompt(self.model);
        NanocodexBuilder::from_openai(self.inner).instructions(prompt)
    }
}

pub(crate) fn system_prompt(model: Model) -> String {
    if matches!(model, Model::MuseWonder10 | Model::MuseWonderFull10) {
        MUSE_SYSTEM_PROMPT.replace("Muse Spark", "Muse Wonder")
    } else {
        MUSE_SYSTEM_PROMPT.to_owned()
    }
}
