//! Reusable configured runtime: shared provider clients and per-harness
//! recipes, constructing many isolated agents with one lifecycle owner.

use std::{
    future::Future,
    path::PathBuf,
    pin::pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

#[cfg(feature = "claude")]
use crate::claude::{ClaudeBuilder, ClaudeClient};
use crate::{
    Agent, AgentEvents, HarnessFamily, HarnessModel, NanocodexError, SessionCheckpoint, Thinking,
    harness::{AgentResult, Harness, HarnessRequest},
};
#[cfg(feature = "openai")]
use crate::{CodexBuilder, OpenAi};
#[cfg(feature = "oai-tools")]
use nanocodex_agent::AgentHandle;
use nanocodex_agent::{
    ModelTransport, SpawnOptions, WeakAgent,
    backend::{AgentFactory, AgentOwner, BackendFuture, adopt},
};

#[cfg(all(feature = "claude", feature = "oai-tools"))]
mod claude_tools;

type Result<T> = nanocodex_agent::Result<T>;
type Recipe = Arc<dyn Fn(AgentSpec) -> BackendFuture<AgentResult> + Send + Sync>;
#[cfg(feature = "openai")]
type CodexHook = Arc<dyn Fn(CodexBuilder) -> Result<CodexBuilder> + Send + Sync>;
#[cfg(feature = "claude")]
type ClaudeHook = Arc<dyn Fn(ClaudeBuilder) -> Result<ClaudeBuilder> + Send + Sync>;
#[cfg(feature = "oai-tools")]
type ToolsFactory = Arc<
    dyn Fn(AgentHandle) -> std::result::Result<crate::Tools, nanocodex_oai_tools::ToolsBuildError>
        + Send
        + Sync,
>;

/// Tool selection materialized independently for every agent.
#[cfg(feature = "oai-tools")]
#[derive(Clone)]
enum AgentTools {
    Shared(crate::Tools),
    PerAgent(ToolsFactory),
}

#[cfg(feature = "oai-tools")]
impl AgentTools {
    fn materialize(
        &self,
        handle: AgentHandle,
    ) -> std::result::Result<crate::Tools, nanocodex_oai_tools::ToolsBuildError> {
        match self {
            Self::Shared(tools) => Ok(tools.clone()),
            Self::PerAgent(factory) => factory(handle),
        }
    }
}

/// Per-agent settings layered spawn > harness > runtime.
#[derive(Clone, Default)]
struct Settings {
    instructions: Option<Arc<str>>,
    workspace: Option<PathBuf>,
    #[cfg(feature = "oai-tools")]
    tools: Option<AgentTools>,
}

impl Settings {
    fn or(self, fallback: &Self) -> Self {
        Self {
            instructions: self.instructions.or_else(|| fallback.instructions.clone()),
            workspace: self.workspace.or_else(|| fallback.workspace.clone()),
            #[cfg(feature = "oai-tools")]
            tools: self.tools.or_else(|| fallback.tools.clone()),
        }
    }
}

macro_rules! settings_methods {
    () => {
        /// Replaces the stable system instructions of agents in this scope.
        #[must_use]
        pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
            self.settings.instructions = Some(instructions.into());
            self
        }

        /// Fixes the working directory agents in this scope run in: their
        /// model context root, tool working directory and rollout workspace.
        /// It does not install file or shell tools; those come from the tools.
        #[must_use]
        pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
            self.settings.workspace = Some(workspace.into());
            self
        }
    };
}

macro_rules! tool_methods {
    () => {
        /// Installs this tool selection, materialized separately for every
        /// agent. Codex installs it natively (its default is the standard
        /// Codex tools); Claude bridges the same tools into its native client
        /// catalog (its default is no client tools).
        #[cfg(feature = "oai-tools")]
        #[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
        #[must_use]
        pub fn tools(mut self, tools: crate::Tools) -> Self {
            self.settings.tools = Some(AgentTools::Shared(tools));
            self
        }

        /// Builds a fresh tool selection for every agent from its weak
        /// capability, for agent-relative tools such as subagent tools.
        #[cfg(feature = "oai-tools")]
        #[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
        #[must_use]
        pub fn tools_factory<T>(mut self, factory: T) -> Self
        where
            T: Fn(
                    AgentHandle,
                )
                    -> std::result::Result<crate::Tools, nanocodex_oai_tools::ToolsBuildError>
                + Send
                + Sync
                + 'static,
        {
            self.settings.tools = Some(AgentTools::PerAgent(Arc::new(factory)));
            self
        }
    };
}

/// Codex recipe: a shared [`OpenAi`] client plus the tools, Code Mode and
/// workspace policy of every Codex agent the runtime constructs.
///
/// The runtime uses the standard Responses service of the client. Hosts with
/// a custom Tower service factory build agents with `Agent::builder` and
/// route children through a host-composed [`crate::Harness`].
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
#[derive(Clone)]
pub struct CodexHarness {
    client: OpenAi,
    settings: Settings,
    code_mode: Option<bool>,
    configure: Option<CodexHook>,
}

#[cfg(feature = "openai")]
impl CodexHarness {
    /// Uses this client, with its authentication, for every Codex agent.
    #[must_use]
    pub fn new(client: OpenAi) -> Self {
        Self {
            client,
            settings: Settings::default(),
            code_mode: None,
            configure: None,
        }
    }

    settings_methods!();
    tool_methods!();

    /// Exposes tools through Code Mode's `exec` (`true`) or as direct
    /// provider tools (`false`). Omitted keeps the tools' own exposure,
    /// Code Mode by default.
    #[must_use]
    pub const fn code_mode(mut self, enabled: bool) -> Self {
        self.code_mode = Some(enabled);
        self
    }

    /// Applies native Codex builder settings to every Codex agent after the
    /// runtime's settings and before any checkpoint is resumed.
    #[must_use]
    pub fn configure<F>(mut self, configure: F) -> Self
    where
        F: Fn(CodexBuilder) -> Result<CodexBuilder> + Send + Sync + 'static,
    {
        self.configure = Some(Arc::new(configure));
        self
    }
}

#[cfg(feature = "openai")]
impl From<OpenAi> for CodexHarness {
    fn from(client: OpenAi) -> Self {
        Self::new(client)
    }
}

/// Claude recipe: a shared [`ClaudeClient`] plus the tools, Code Mode and
/// workspace policy of every Claude agent the runtime constructs.
#[cfg(feature = "claude")]
#[cfg_attr(docsrs, doc(cfg(feature = "claude")))]
#[derive(Clone)]
pub struct ClaudeHarness {
    client: ClaudeClient,
    settings: Settings,
    code_mode: bool,
    configure: Option<ClaudeHook>,
}

#[cfg(feature = "claude")]
impl ClaudeHarness {
    /// Uses this client, with its API key or subscription authentication,
    /// for every Claude agent.
    #[must_use]
    pub fn new(client: ClaudeClient) -> Self {
        Self {
            client,
            settings: Settings::default(),
            code_mode: true,
            configure: None,
        }
    }

    settings_methods!();
    tool_methods!();

    /// Exposes tools through Code Mode's `exec` and `wait` (`true`, the
    /// default) or as direct Claude client tools (`false`).
    #[must_use]
    pub const fn code_mode(mut self, enabled: bool) -> Self {
        self.code_mode = enabled;
        self
    }

    /// Applies native Claude builder settings to every Claude agent after
    /// the runtime's settings and before any checkpoint is resumed.
    #[must_use]
    pub fn configure<F>(mut self, configure: F) -> Self
    where
        F: Fn(ClaudeBuilder) -> Result<ClaudeBuilder> + Send + Sync + 'static,
    {
        self.configure = Some(Arc::new(configure));
        self
    }
}

#[cfg(feature = "claude")]
impl From<ClaudeClient> for ClaudeHarness {
    fn from(client: ClaudeClient) -> Self {
        Self::new(client)
    }
}

/// Configures a reusable [`Nanocodex`] runtime.
#[derive(Clone, Default)]
pub struct NanocodexBuilder {
    #[cfg(feature = "openai")]
    codex: Option<CodexHarness>,
    #[cfg(feature = "claude")]
    claude: Option<ClaudeHarness>,
    harness: Option<HarnessFamily>,
    model: Option<HarnessModel>,
    thinking: Option<Thinking>,
    settings: Settings,
}

impl NanocodexBuilder {
    /// Configures the Codex harness from a client or a [`CodexHarness`].
    #[cfg(feature = "openai")]
    #[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
    #[must_use]
    pub fn openai(mut self, harness: impl Into<CodexHarness>) -> Self {
        self.codex = Some(harness.into());
        self
    }

    /// Configures the Claude harness from a client or a [`ClaudeHarness`].
    #[cfg(feature = "claude")]
    #[cfg_attr(docsrs, doc(cfg(feature = "claude")))]
    #[must_use]
    pub fn claude(mut self, harness: impl Into<ClaudeHarness>) -> Self {
        self.claude = Some(harness.into());
        self
    }

    /// Selects the default family of spawned agents.
    #[must_use]
    pub const fn harness(mut self, family: HarnessFamily) -> Self {
        self.harness = Some(family);
        self
    }

    /// Selects the default model of spawned agents; its family is inferred.
    #[must_use]
    pub fn model(mut self, model: impl Into<HarnessModel>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Selects the default thinking level of spawned agents.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = Some(thinking);
        self
    }

    settings_methods!();

    /// Validates the configuration and freezes it into a reusable runtime.
    ///
    /// Without a family or model, agents use Codex when it is configured and
    /// otherwise the sole configured family, with its default model and that
    /// model's default thinking.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidRequest`] when no harness is
    /// configured, the selected family is not configured, the model belongs
    /// to another family, or the thinking level is unsupported by the model.
    pub fn build(self) -> Result<Nanocodex> {
        let mut configured = Vec::new();
        #[cfg(feature = "openai")]
        if self.codex.is_some() {
            configured.push(HarnessFamily::Codex);
        }
        #[cfg(feature = "claude")]
        if self.claude.is_some() {
            configured.push(HarnessFamily::Claude);
        }
        let Some(&fallback) = configured.first() else {
            return Err(NanocodexError::InvalidRequest(
                "a Nanocodex runtime requires at least one harness backend".into(),
            ));
        };
        if let (Some(family), Some(model)) = (self.harness, self.model)
            && family != model.family()
        {
            return Err(NanocodexError::InvalidRequest(
                "model does not belong to selected harness".into(),
            ));
        }
        let family = self
            .harness
            .or_else(|| self.model.map(HarnessModel::family))
            .unwrap_or(fallback);
        let model = self.model.unwrap_or_else(|| family.default_model());
        let thinking = self.thinking.unwrap_or_else(|| model.default_thinking());
        model
            .capabilities(ModelTransport::Native)
            .check_thinking(thinking)?;
        if !configured.contains(&family) {
            return Err(not_configured(family));
        }

        let ownership = Arc::new(Ownership::default());
        let mut recipes: Vec<(HarnessFamily, Recipe)> = Vec::new();
        #[cfg(feature = "openai")]
        if let Some(codex) = self.codex {
            recipes.push((HarnessFamily::Codex, codex_recipe(codex, &self.settings)));
        }
        #[cfg(feature = "claude")]
        if let Some(claude) = self.claude {
            recipes.push((HarnessFamily::Claude, claude_recipe(claude, &self.settings)));
        }
        let mut router = Harness::builder();
        for (family, recipe) in &recipes {
            let recipe = Arc::clone(recipe);
            router = router.register(*family, move |request: HarnessRequest| {
                recipe(AgentSpec::routed(request))
            });
        }
        Ok(Nanocodex {
            inner: Arc::new(Runtime {
                harness: router.build_tracked(Arc::downgrade(&ownership)),
                recipes,
                model,
                thinking,
                ownership,
            }),
        })
    }
}

/// A reusable configured runtime that constructs isolated agents from shared
/// provider clients and per-harness recipes.
///
/// Every agent gets its own conversation, tool runtime and event stream; the
/// clients and their authentication are shared. Agents constructed by the
/// runtime—roots, resumed sessions, siblings and forks created from them—stay
/// owned by it until they are shut down, even if their handles are dropped.
/// [`Self::shutdown`] stops admitting agents and shuts down every agent it
/// still owns, plus live subagents routed through its recipes. Dropping the
/// last runtime clone releases that ownership without stopping agents whose
/// handles are still held.
///
/// ```no_run
/// # use nanocodex::{ClaudeModel, Nanocodex, OpenAi, claude::ClaudeClient};
/// # async fn example(openai: OpenAi, claude: ClaudeClient) -> nanocodex::agent::Result<()> {
/// let nano = Nanocodex::builder().openai(openai).claude(claude).build()?;
/// let (codex, _events) = nano.spawn().instructions("Review the diff.").build().await?;
/// let (sonnet, _events) = nano.spawn().model(ClaudeModel::Sonnet55).build().await?;
/// codex.prompt("Summarize the open issues.").await?.result().await?;
/// let saved = sonnet.checkpoint().await?;
/// let (_resumed, _events) = nano.resume(saved).await?;
/// nano.shutdown().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Nanocodex {
    inner: Arc<Runtime>,
}

struct Runtime {
    harness: Harness,
    recipes: Vec<(HarnessFamily, Recipe)>,
    model: HarnessModel,
    thinking: Thinking,
    ownership: Arc<Ownership>,
}

impl Nanocodex {
    /// Starts configuring a runtime.
    #[must_use]
    pub fn builder() -> NanocodexBuilder {
        NanocodexBuilder::default()
    }

    /// Starts configuring one agent from the runtime defaults.
    pub fn spawn(&self) -> SpawnBuilder<'_> {
        SpawnBuilder {
            runtime: self,
            options: SpawnOptions::new(),
            settings: Settings::default(),
        }
    }

    /// Default family of spawned agents.
    #[must_use]
    pub fn harness(&self) -> HarnessFamily {
        self.inner.model.family()
    }

    /// Default model of spawned agents.
    #[must_use]
    pub fn model(&self) -> HarnessModel {
        self.inner.model
    }

    /// Default thinking level of spawned agents.
    #[must_use]
    pub fn thinking(&self) -> Thinking {
        self.inner.thinking
    }

    /// Whether a family's backend is configured.
    #[must_use]
    pub fn is_configured(&self, family: HarnessFamily) -> bool {
        self.inner.recipe(family).is_ok()
    }

    /// The cross-family child router installed on every agent, for hosts
    /// composing their own native builders with this runtime's recipes.
    #[must_use]
    pub fn spawn_factory(&self) -> Arc<dyn AgentFactory> {
        self.inner.harness.spawn_factory()
    }

    /// Reopens a checkpointed session through its family's recipe, with the
    /// runtime's current credentials, tools, instructions and workspace.
    ///
    /// The session keeps its identity, lineage, model, thinking level and
    /// committed history. Per-spawn overrides are not part of a checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidCheckpoint`] for an invalid
    /// checkpoint, [`NanocodexError::InvalidRequest`] when its family is not
    /// configured, [`NanocodexError::AgentStopped`] after shutdown, or the
    /// recipe's construction error.
    pub async fn resume(&self, checkpoint: SessionCheckpoint) -> AgentResult {
        checkpoint.validate()?;
        let model = checkpoint.model();
        let thinking = checkpoint.thinking();
        self.construct(AgentSpec {
            model,
            thinking,
            settings: Settings::default(),
            spawn_factory: self.spawn_factory(),
            host_context: None,
            checkpoint: Some(checkpoint),
            #[cfg(feature = "durability")]
            durable_state: None,
        })
        .await
    }

    /// Reopens a durable catalog session by ID through its family's recipe;
    /// the session owns its durable state and records later turns.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidRequest`] for an unknown session or
    /// unconfigured family, [`NanocodexError::Backend`] for a catalog
    /// failure, or the recipe's construction error.
    #[cfg(feature = "durability")]
    #[cfg_attr(docsrs, doc(cfg(feature = "durability")))]
    pub async fn open(
        &self,
        store: &crate::durability::SessionStore,
        session_id: &str,
    ) -> AgentResult {
        let stored = store.load(session_id).await.map_err(catalog_error)?;
        let model = stored.summary.record.model;
        let thinking = stored
            .session_checkpoint()
            .map_err(catalog_error)?
            .map_or_else(
                || model.default_thinking(),
                |checkpoint| checkpoint.thinking(),
            );
        self.inner.recipe(model.family())?;
        let durable_state = store.resume(session_id).await.map_err(catalog_error)?;
        self.construct(AgentSpec {
            model,
            thinking,
            settings: Settings::default(),
            spawn_factory: self.spawn_factory(),
            host_context: None,
            checkpoint: None,
            durable_state: Some(durable_state),
        })
        .await
    }

    /// Stops admitting agents, waits for constructions already admitted, and
    /// shuts down every agent the runtime owns and every live subagent routed
    /// through its recipes. Concurrent and later calls share the result.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::Backend`] describing every agent whose
    /// shutdown failed; all other agents are still shut down.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.ownership.shutdown().await
    }

    async fn construct(&self, spec: AgentSpec) -> AgentResult {
        let recipe = self.inner.recipe(spec.model.family())?;
        let ownership = Arc::clone(&self.inner.ownership);
        ownership.own(move || recipe(spec)).await
    }
}

impl Runtime {
    fn recipe(&self, family: HarnessFamily) -> Result<Recipe> {
        self.recipes
            .iter()
            .find(|(configured, _)| *configured == family)
            .map(|(_, recipe)| Arc::clone(recipe))
            .ok_or_else(|| not_configured(family))
    }
}

/// Configures one agent; omitted settings use the runtime defaults.
#[must_use = "call build().await to construct the agent"]
pub struct SpawnBuilder<'a> {
    runtime: &'a Nanocodex,
    options: SpawnOptions,
    settings: Settings,
}

impl SpawnBuilder<'_> {
    /// Selects this agent's family. Selecting another family than the
    /// runtime default uses that family's default model.
    pub const fn harness(mut self, family: HarnessFamily) -> Self {
        self.options = self.options.harness(family);
        self
    }

    /// Selects this agent's model; its family is inferred. A model other
    /// than the runtime default uses its own default thinking.
    pub fn model(mut self, model: impl Into<HarnessModel>) -> Self {
        self.options = self.options.harness_model(model.into());
        self
    }

    /// Selects this agent's thinking level.
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.options = self.options.thinking(thinking);
        self
    }

    /// Replaces this agent's stable system instructions.
    pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.settings.instructions = Some(instructions.into());
        self
    }

    /// Fixes this agent's working directory and model context root.
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.settings.workspace = Some(workspace.into());
        self
    }

    /// Constructs the agent through its family's recipe.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidRequest`] for a model of another
    /// family, an unsupported thinking level or an unconfigured family, all
    /// before any provider request; [`NanocodexError::AgentStopped`] after
    /// shutdown; or the recipe's construction error.
    pub async fn build(self) -> AgentResult {
        let runtime = &self.runtime.inner;
        let resolved = self.options.resolve(runtime.model, runtime.thinking)?;
        let model = resolved.selected_harness_model().unwrap_or(runtime.model);
        let thinking = resolved.selected_thinking().unwrap_or(runtime.thinking);
        self.runtime
            .construct(AgentSpec {
                model,
                thinking,
                settings: self.settings,
                spawn_factory: self.runtime.spawn_factory(),
                host_context: None,
                checkpoint: None,
                #[cfg(feature = "durability")]
                durable_state: None,
            })
            .await
    }
}

/// Complete construction input for one recipe invocation.
struct AgentSpec {
    model: HarnessModel,
    thinking: Thinking,
    settings: Settings,
    spawn_factory: Arc<dyn AgentFactory>,
    host_context: Option<Arc<str>>,
    checkpoint: Option<SessionCheckpoint>,
    #[cfg(feature = "durability")]
    durable_state: Option<crate::durability::DurableSession>,
}

impl AgentSpec {
    /// A child or restoration routed through the shared registry takes the
    /// runtime's harness settings, never a sibling's per-spawn overrides.
    fn routed(request: HarnessRequest) -> Self {
        Self {
            model: request.model,
            thinking: request.thinking,
            settings: Settings::default(),
            spawn_factory: request.spawn_factory,
            host_context: request.host_context,
            checkpoint: request.checkpoint,
            #[cfg(feature = "durability")]
            durable_state: request.durable_state,
        }
    }
}

#[cfg(feature = "oai-tools")]
fn exposed(
    tools: crate::Tools,
    code_mode: Option<bool>,
) -> std::result::Result<crate::Tools, nanocodex_oai_tools::ToolsBuildError> {
    use nanocodex_oai_tools::ToolExposure;
    match code_mode {
        None => Ok(tools),
        Some(code_mode) => tools
            .into_builder()
            .exposure(if code_mode {
                ToolExposure::CodeModeOnly
            } else {
                ToolExposure::DirectOnly
            })
            .build(),
    }
}

#[cfg(feature = "openai")]
fn codex_recipe(harness: CodexHarness, runtime: &Settings) -> Recipe {
    let CodexHarness {
        client,
        settings,
        code_mode,
        configure,
    } = harness;
    let settings = settings.or(runtime);
    Arc::new(move |spec: AgentSpec| {
        let client = client.clone();
        let settings = spec.settings.or(&settings);
        let configure = configure.clone();
        Box::pin(async move {
            let HarnessModel::Codex(model) = spec.model else {
                return Err(NanocodexError::InvalidRequest(
                    "the Codex harness requires a Codex model".into(),
                ));
            };
            let mut builder = Agent::builder(client)
                .model(model)
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
            {
                let tools = settings
                    .tools
                    .unwrap_or_else(|| AgentTools::Shared(crate::Tools::default()));
                builder = builder
                    .tools_factory(move |handle| exposed(tools.materialize(handle)?, code_mode));
            }
            #[cfg(not(feature = "oai-tools"))]
            let _ = code_mode;
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

#[cfg(feature = "claude")]
fn claude_recipe(harness: ClaudeHarness, runtime: &Settings) -> Recipe {
    let ClaudeHarness {
        client,
        settings,
        code_mode,
        configure,
    } = harness;
    let settings = settings.or(runtime);
    Arc::new(move |spec: AgentSpec| {
        let client = client.clone();
        let settings = spec.settings.or(&settings);
        let configure = configure.clone();
        Box::pin(async move {
            if spec.model.family() != HarnessFamily::Claude {
                return Err(NanocodexError::InvalidRequest(
                    "the Claude harness requires a Claude model".into(),
                ));
            }
            let mut builder = Agent::builder(crate::Claude::new(client, spec.model.as_str()))
                .thinking(spec.thinking)?
                .spawn_factory(spec.spawn_factory)
                .host_context(spec.host_context);
            if let Some(instructions) = settings.instructions {
                builder = builder.system(instructions.to_string());
            }
            if let Some(workspace) = &settings.workspace {
                builder = builder.workspace(workspace.to_string_lossy().into_owned());
            }
            // Claude keeps its native default of no client tools; configured
            // tools are bridged. OpenAI-hosted tools have no local handler.
            #[cfg(feature = "oai-tools")]
            if let Some(tools) = settings.tools {
                let root = match settings.workspace {
                    Some(workspace) => workspace,
                    None => {
                        // Tools and model context agree on one directory.
                        let current = std::env::current_dir().map_err(|error| {
                            NanocodexError::InvalidRequest(format!(
                                "Claude tools require a workspace: {error}"
                            ))
                        })?;
                        builder = builder.workspace(current.to_string_lossy().into_owned());
                        current
                    }
                };
                builder = builder.code_only(code_mode).tools_factory(move |handle| {
                    let selected = tools
                        .materialize(handle.clone())
                        .and_then(|tools| exposed(tools, Some(code_mode)))
                        .map_err(|error| NanocodexError::InvalidRequest(error.to_string()))?;
                    claude_tools::install(&selected, code_mode, root.clone(), &handle)
                });
            }
            #[cfg(not(feature = "oai-tools"))]
            let _ = code_mode;
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

fn not_configured(family: HarnessFamily) -> NanocodexError {
    NanocodexError::InvalidRequest(format!(
        "{family} harness is not configured for this runtime"
    ))
}

#[cfg(feature = "durability")]
fn catalog_error(error: crate::durability::Error) -> NanocodexError {
    match error {
        crate::durability::Error::SessionNotFound { .. } => {
            NanocodexError::InvalidRequest(error.to_string())
        }
        error => NanocodexError::Backend {
            backend: "durability",
            source: Arc::new(error),
        },
    }
}

/// Admission gate and owner of every agent a runtime constructs.
#[derive(Default)]
pub(crate) struct Ownership {
    state: Mutex<OwnershipState>,
    idle: tokio::sync::Notify,
    stopped: tokio::sync::OnceCell<Option<Arc<ShutdownError>>>,
}

#[derive(Default)]
struct OwnershipState {
    closed: bool,
    drained: bool,
    in_flight: usize,
    owned: Vec<(usize, Agent)>,
    children: Vec<WeakAgent>,
    failures: Vec<String>,
}

/// Ends one admitted construction even when its future is dropped.
struct Admission<'a>(&'a Ownership);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

impl Ownership {
    fn lock(&self) -> MutexGuard<'_, OwnershipState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn admission(&self) -> Result<Admission<'_>> {
        self.admit()?;
        Ok(Admission(self))
    }

    /// Admits and constructs one root, then owns it.
    async fn own<F, Fut>(self: Arc<Self>, construct: F) -> AgentResult
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AgentResult>,
    {
        let admission = self.admission()?;
        let (agent, events) = construct().await?;
        let owner: Arc<dyn AgentOwner> = self.clone();
        let agent = adopt(agent, &owner).await?;
        drop(admission);
        Ok((agent, events))
    }

    /// Admits one routed child construction and tracks its results weakly:
    /// the subagent registry that receives them owns and evicts them.
    pub(crate) async fn track_children<F>(&self, construct: F) -> Result<Vec<(Agent, AgentEvents)>>
    where
        F: Future<Output = Result<Vec<(Agent, AgentEvents)>>>,
    {
        let admission = self.admission()?;
        let created = construct.await?;
        let mut state = self.lock();
        state.children.retain(WeakAgent::is_live);
        state
            .children
            .extend(created.iter().map(|(agent, _)| agent.downgrade()));
        drop(state);
        drop(admission);
        Ok(created)
    }

    async fn shutdown(&self) -> Result<()> {
        let outcome = self
            .stopped
            .get_or_init(|| async {
                self.lock().closed = true;
                loop {
                    let mut idle = pin!(self.idle.notified());
                    idle.as_mut().enable();
                    if self.lock().in_flight == 0 {
                        break;
                    }
                    idle.await;
                }
                self.lock().drained = true;
                // Agents leave the registry only once their shutdown returns,
                // so a dropped shutdown future leaves the rest for the next
                // call to join. Newest first: forks and siblings before their
                // sources, then live routed children the registry still holds.
                loop {
                    let next = self.lock().owned.last().cloned();
                    let Some((key, agent)) = next else { break };
                    let result = agent.shutdown().await;
                    let mut state = self.lock();
                    state.owned.retain(|(owned, _)| *owned != key);
                    if let Err(error) = result {
                        state.failures.push(error.to_string());
                    }
                }
                loop {
                    let next = self.lock().children.last().cloned();
                    let Some(child) = next else { break };
                    let result = child.shutdown().await;
                    let mut state = self.lock();
                    state.children.pop();
                    if let Some(Err(error)) = result {
                        state.failures.push(error.to_string());
                    }
                }
                let failures = std::mem::take(&mut self.lock().failures);
                (!failures.is_empty()).then(|| Arc::new(ShutdownError(failures)))
            })
            .await;
        match outcome {
            None => Ok(()),
            Some(error) => Err(NanocodexError::Backend {
                backend: "runtime",
                source: Arc::clone(error) as Arc<dyn std::error::Error + Send + Sync>,
            }),
        }
    }
}

impl AgentOwner for Ownership {
    fn admit(&self) -> Result<()> {
        let mut state = self.lock();
        if state.closed {
            return Err(NanocodexError::AgentStopped);
        }
        state.in_flight += 1;
        Ok(())
    }

    fn finish(&self) {
        let mut state = self.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.idle.notify_waiters();
        }
    }

    fn adopt(&self, key: usize, agent: Agent) -> bool {
        let mut state = self.lock();
        if state.drained {
            return false;
        }
        state.owned.push((key, agent));
        true
    }

    fn release(&self, key: usize) {
        self.lock().owned.retain(|(owned, _)| *owned != key);
    }
}

/// Every agent shutdown failure observed by [`Nanocodex::shutdown`].
#[derive(Debug)]
struct ShutdownError(Vec<String>);

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} agent(s) failed to shut down: {}",
            self.0.len(),
            self.0.join("; ")
        )
    }
}

impl std::error::Error for ShutdownError {}
