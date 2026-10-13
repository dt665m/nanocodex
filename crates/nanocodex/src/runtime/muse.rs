//! Muse recipes for the reusable runtime's Muse family.
use super::*;
use crate::{Model, Muse};

type MuseBuilder = nanocodex_muse::NanocodexBuilder<nanocodex_muse::MuseServiceFactory>;
type MuseHook = Arc<dyn Fn(MuseBuilder) -> Result<MuseBuilder> + Send + Sync>;

/// Muse recipe: HTTP/SSE Responses, with Muse-owned auth, images and compaction.
/// Image generation is disabled unless configured tools explicitly enable it.
#[derive(Clone)]
pub struct MuseHarness {
    client: Muse,
    settings: Settings,
    configure: Option<MuseHook>,
}

impl MuseHarness {
    /// Shares this provider's authentication and HTTP client across agents.
    #[must_use]
    pub fn new(client: Muse) -> Self {
        Self {
            client,
            settings: Settings::default(),
            configure: None,
        }
    }
    settings_methods!();
    tool_methods!();

    pub(super) fn model(&self) -> Model {
        self.client.selected_model()
    }

    /// Applies Muse-native settings after runtime settings and before resume.
    #[must_use]
    pub fn configure<F>(mut self, configure: F) -> Self
    where
        F: Fn(MuseBuilder) -> Result<MuseBuilder> + Send + Sync + 'static,
    {
        self.configure = Some(Arc::new(configure));
        self
    }
}

impl From<Muse> for MuseHarness {
    fn from(client: Muse) -> Self {
        Self::new(client)
    }
}

pub(super) fn recipe(harness: MuseHarness, runtime: &Settings) -> Recipe {
    let MuseHarness {
        client,
        settings,
        configure,
    } = harness;
    let settings = settings.or(runtime);
    Arc::new(move |spec: AgentSpec| {
        let client = client.clone();
        let settings = spec.settings.or(&settings);
        let configure = configure.clone();
        Box::pin(async move {
            let HarnessModel::Muse(model) = spec.model else {
                return Err(NanocodexError::InvalidRequest(
                    "the Muse harness requires a Muse model".into(),
                ));
            };
            let mut builder = Agent::builder(client)
                .model(model.into())
                .thinking(spec.thinking)
                .spawn_factory(spec.spawn_factory)
                .host_context(spec.host_context);
            if let Some(instructions) = settings.instructions {
                builder = builder.instructions(instructions);
            }
            if let Some(workspace) = settings.workspace {
                builder = builder.workspace(workspace);
            }
            #[cfg(feature = "oai-tools")]
            if let Some(tools) = settings.tools {
                builder = builder.tools_factory(move |handle| tools.materialize(handle));
            }
            if let Some(configure) = configure {
                builder = configure(builder)?;
            }
            if let Some(checkpoint) = spec.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            #[cfg(feature = "durability")]
            if let Some(state) = spec.durable_state {
                use crate::DurableAgentExt as _;
                builder = builder.durability(state).await?;
            }
            builder.build()
        })
    })
}
