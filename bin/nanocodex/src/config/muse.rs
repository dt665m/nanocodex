//! Muse's native recipe over the CLI's shared Responses host capabilities.
use super::*;
use nanocodex::{Muse, MuseModel, muse::MuseServiceFactory};

type MuseBuilder = nanocodex::muse::NanocodexBuilder<MuseServiceFactory>;

#[derive(Clone)]
pub(super) struct MuseConnection {
    auth: crate::auth::MuseAuthArgs,
    client: Arc<tokio::sync::OnceCell<Muse>>,
}

impl MuseConnection {
    fn new(auth: crate::auth::MuseAuthArgs) -> Self {
        Self {
            auth,
            client: Arc::default(),
        }
    }
    async fn client(&self) -> std::result::Result<Muse, String> {
        self.client
            .get_or_try_init(|| async {
                self.auth
                    .clone()
                    .client()
                    .await
                    .map_err(|error| error.to_string())
            })
            .await
            .cloned()
    }
}

/// Reuses the Codex host policy, tools and workspace registry; only the provider differs.
#[derive(Clone)]
pub(super) struct MuseRecipe {
    connection: MuseConnection,
    host: codex::CodexRecipe,
    image_generation: bool,
}

impl MuseRecipe {
    pub(super) fn new(
        args: &AgentArgs,
        host: codex::CodexRecipe,
        connection: Option<MuseConnection>,
    ) -> Self {
        Self {
            connection: connection.unwrap_or_else(|| MuseConnection::new(args.muse_auth.clone())),
            host,
            image_generation: args.image_generation.unwrap_or(false),
        }
    }

    fn builder(
        &self,
        client: Muse,
        session_id: SessionId,
        workspace: PathBuf,
    ) -> Result<MuseBuilder> {
        let tools = self
            .host
            .tools
            .clone()
            .into_builder()
            // Codex's hosted web search has no Muse adapter. MCP tools remain available.
            .web_search(false)
            .image_generation(self.image_generation)
            .build()?;
        let registry = self.host.registry.clone();
        let workspaces = self.host.workspaces.clone();
        let tool_workspace = workspace.clone();
        let mut builder = Agent::builder(client)
            .session_id(session_id)
            .workspace(workspace)
            .codex_home(self.host.codex_home.clone())
            .tools_factory(move |agent| {
                workspaces
                    .seed(agent.session_id(), tool_workspace.clone())
                    .map_err(nanocodex::tools::runtime::ToolsBuildError::HostInitialization)?;
                if let Some(registry) = &registry {
                    nanocodex_subagents::install_tools(tools.clone(), agent, Arc::clone(registry))
                } else {
                    Ok(tools.clone())
                }
            });
        if let Some(home) = crate::homes::resolve() {
            builder = builder.claude_home(home.claude_home());
        }
        if let Some(instructions) = self.host.instructions.clone() {
            builder = builder.instructions(instructions);
        }
        if let Some(instructions) = self.host.additional_instructions.clone() {
            builder = builder.additional_instructions(instructions);
        }
        Ok(builder)
    }
}

pub(super) fn register_muse_recipe(
    harness: nanocodex::HarnessBuilder,
    recipe: MuseRecipe,
) -> nanocodex::HarnessBuilder {
    harness.register(HarnessFamily::Muse, move |request| {
        let recipe = recipe.clone();
        async move {
            let invalid = nanocodex::NanocodexError::InvalidRequest;
            let HarnessModel::Muse(model) = request.model else {
                return Err(invalid("Muse recipe requires a Muse model".into()));
            };
            recipe
                .host
                .workspaces
                .authorize_cross_family(request.parent.as_ref())
                .map_err(invalid)?;
            let session_id = match (&request.checkpoint, &request.durable_state) {
                (Some(checkpoint), _) => checkpoint.session_id().parse::<SessionId>(),
                (None, Some(state)) => state.state_id().parse::<SessionId>(),
                (None, None) => Ok(SessionId::new()),
            }
            .map_err(|error| invalid(error.to_string()))?;
            let session_key = session_id.to_string();
            if let Some(parent) = &request.parent {
                recipe
                    .host
                    .workspaces
                    .initialize(parent.session_id(), &session_key)
            } else {
                recipe
                    .host
                    .workspaces
                    .seed(&session_key, recipe.host.workspace.clone())
            }
            .map_err(invalid)?;
            let workspace = recipe
                .host
                .workspaces
                .current(&session_key)
                .map_err(invalid)?;
            let client = recipe.connection.client().await.map_err(invalid)?;
            let mut builder = recipe
                .builder(client, session_id, workspace)
                .map_err(|error| invalid(error.to_string()))?
                .model(model.into())
                .thinking(request.thinking)
                .spawn_factory(request.spawn_factory)
                .host_context(request.host_context);
            if let Some(checkpoint) = request.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            if let Some(state) = request.durable_state {
                builder = builder.durability(state).await?;
            }
            builder.build()
        }
    })
}

impl AgentArgs {
    pub(super) async fn build_muse(
        self,
        root: RootSession,
        codex_home: PathBuf,
        vm: VmArgs,
        tui: bool,
        requested_model: Option<HarnessModel>,
    ) -> Result<ConfiguredAgent> {
        if vm.is_enabled() {
            return Err(eyre!("--vm requires the Codex harness"));
        }
        if self.mpp.is_enabled() {
            return Err(eyre!("the Tempo provider requires the Codex harness"));
        }
        if self
            .responses_transport
            .is_some_and(|transport| !matches!(transport, ResponsesTransport::Https))
            || self.websocket_url.is_some()
        {
            return Err(eyre!("Muse supports HTTP/SSE Responses, not WebSockets"));
        }
        if self.model_policy.web_search == Some(true) {
            return Err(eyre!(
                "the shared web-search tool uses Codex endpoints; configure a web MCP tool for Muse"
            ));
        }
        let model = requested_model.unwrap_or(HarnessModel::Muse(MuseModel::default()));
        let HarnessModel::Muse(native_model) = model else {
            unreachable!("validated Muse model")
        };
        let thinking = self
            .model_policy
            .requested_thinking(HarnessFamily::Muse)?
            .unwrap_or_else(|| model.default_thinking());
        let connection = MuseConnection::new(self.muse_auth.clone());
        let client = connection.client().await.map_err(|error| eyre!(error))?;
        let managed_memory = self.managed_memory(&codex_home, &root.session_id).await?;
        let (catalog, mcp_handle) = self
            .host_tools(&codex_home, None, None, managed_memory.as_ref())
            .await?;
        let registry = self
            .subagents
            .then(|| subagents::channel(self.max_subagents));
        let tool_registry = registry
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let workspaces = Arc::new(claude::WorkspaceRegistry::new(
            root.workspace.clone(),
            codex_home.clone(),
        ));
        let responses = self.responses_settings();
        let codex_auth = self.auth.clone();
        let host = self.codex_recipe(
            HarnessFamily::Muse,
            codex::CodexConnection::lazy(move || {
                responses.client(codex_auth.clone().resolve()?.nanocodex()?, None)
            }),
            catalog.clone(),
            &codex_home,
            &root.workspace,
            &workspaces,
            tool_registry.clone(),
            managed_memory.is_some(),
        );
        let recipe = MuseRecipe::new(&self, host.clone(), Some(connection.clone()));
        let harness = self
            .harness(
                host,
                self.claude_connection()?,
                Some(connection),
                &catalog,
                &root.workspace,
                tool_registry,
                mcp_handle.clone(),
                &workspaces,
            )?
            .build();
        let session_id = root
            .session_id
            .parse::<SessionId>()
            .wrap_err("Muse session IDs must be UUIDv7")?;
        let mut builder = recipe
            .builder(client, session_id, root.workspace.clone())?
            .model(native_model.into())
            .thinking(thinking)
            .spawn_factory(harness.spawn_factory());
        if let Some(persistence) = &root.persistence {
            if persistence.mirror().is_some() {
                let mirror = nanocodex::muse::rollout::RolloutConfig::new(&codex_home);
                let mirror = mirror.recorded(&root.session_id)?.unwrap_or(mirror);
                builder = builder.rollout(mirror);
            }
            builder = builder
                .durability(persistence.open(model, &root.workspace).await?)
                .await
                .wrap_err("failed to attach session durability")?;
        }
        let (handle, events) = builder.build()?;
        let (child_agents, subagent_updates) = child_agents(&handle, registry, tui);
        let realtime = self
            .auth
            .clone()
            .resolve()
            .and_then(|auth| auth.nanocodex())
            .and_then(|auth| Ok(OpenAi::new(auth)?))
            .ok();
        Ok(ConfiguredAgent {
            host: HostChannels::default(),
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter: None,
            mcp: mcp_handle,
            vm: None,
            model,
        })
    }
}
