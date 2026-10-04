//! Native durable composition, including owned task-tree tools and descendants.
use nanocodex_agent::{
    AgentEvents, AgentHandle, ChildSnapshot, HarnessModel, Nanocodex, NanocodexBuilder,
    NanocodexError, SpawnOptions, Thinking,
    backend::{AgentFactory, BackendFuture},
    session::SessionId,
};
use nanocodex_durability::{ChildJournal, DurableSession};
use nanocodex_oai_api::{
    ResponseError,
    __private::ResponsesServiceFactory,
    tower::{ResponsesAttempt, ResponsesServiceResponse},
};
use nanocodex_subagents::{Registry, RegistryOwnership, channel, install_tools};
use std::{future::Future, sync::Arc};
use tower::Service;

type Result<T> = nanocodex_agent::Result<T>;

/// Attaches durable native execution and its owned child tree automatically.
pub trait DurableAgentExt: Sized {
    /// Restores the native conversation, child topology and per-child journals.
    /// Existing caller tools are retained and child lifecycle tools are installed.
    fn durability(self, state: DurableSession) -> impl Future<Output = Result<Self>>;
}

impl<F> DurableAgentExt for NanocodexBuilder<F>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    async fn durability(self, state: DurableSession) -> Result<Self> {
        // Native session identity survives a process replacement even when the
        // host uses a non-UUID store key and supplies no explicit session ID.
        let mut identity = ChildJournal::open(
            state.child_store(),
            &format!("{}/native-root", state.state_id()),
        )
        .await
        .map_err(error)?;
        let saved = identity.load::<String>().await.map_err(error)?;
        let session = match saved {
            Some(saved) => {
                let saved: SessionId = saved.parse().map_err(error)?;
                if self.configured_session_id().is_some_and(|id| id != saved) {
                    return Err(NanocodexError::InvalidSessionSnapshot(
                        "configured native identity differs from durable root".into(),
                    ));
                }
                saved
            }
            None => {
                let session = self.configured_session_id().unwrap_or_default();
                identity.commit(&session.to_string()).await.map_err(error)?;
                session
            }
        };
        let (registry, _, mut updates) = channel(usize::MAX);
        // Registry publication must never stall on an unconsumed event queue.
        tokio::spawn(async move { while updates.recv().await.is_some() {} });
        registry
            .enable_durability(state.child_store(), &session.to_string())
            .await
            .map_err(error)?;
        let factory = Arc::new(NativeChildren {
            recipe: self.clone().fresh_child(),
            state: state.clone(),
            registry: registry.clone(),
        });
        let builder = self
            .session_id(session)
            .spawn_factory(factory)
            .turn_ownership(Arc::new(RegistryOwnership(registry.clone())))
            .map_tools_factory(move |handle, tools| install_tools(tools, handle, registry.clone()));
        nanocodex_durability::DurableAgentExt::durability(builder, state).await
    }
}

struct NativeChildren<F> {
    recipe: NanocodexBuilder<F>,
    state: DurableSession,
    registry: Arc<Registry>,
}

impl<F> NativeChildren<F>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    async fn construct(
        self: Arc<Self>,
        parent: AgentHandle,
        options: SpawnOptions,
        context: Option<Arc<str>>,
        snapshot: Option<ChildSnapshot>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        parent.ensure_available().await?;
        let (model, thinking) = parent.settings().await?;
        let options = options.resolve(model, thinking)?;
        let HarnessModel::Codex(model) = options.selected_harness_model().expect("resolved") else {
            return Err(NanocodexError::InvalidRequest(
                "native durable recipe requires a configured Claude recipe for a family switch"
                    .into(),
            ));
        };
        let mut builder = self
            .recipe
            .clone()
            .fresh_child()
            .model(model)
            .thinking(options.selected_thinking().expect("resolved"))
            .host_context(context)
            .spawn_factory(self.clone())
            .turn_ownership(Arc::new(RegistryOwnership(self.registry.clone())));
        let session = match snapshot {
            Some(snapshot) => {
                let session = match &snapshot {
                    ChildSnapshot::Codex(snapshot) => snapshot.session_id.parse().map_err(error)?,
                    _ => unreachable!(),
                };
                builder = builder.restore_runtime(snapshot)?;
                session
            }
            None => SessionId::new(),
        };
        let registry = self.registry.clone();
        builder = builder
            .session_id(session)
            .map_tools_factory(move |handle, tools| install_tools(tools, handle, registry.clone()));
        let state = DurableSession::open(
            self.state.child_store(),
            format!("{}/child/{session}", self.state.state_id()),
        )
        .await
        .map_err(error)?;
        let builder = nanocodex_durability::DurableAgentExt::durability(builder, state).await?;
        builder.build()
    }
}

impl<F> AgentFactory for NativeChildren<F>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    fn spawn(
        &self,
        parent: AgentHandle,
        options: SpawnOptions,
        context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let recipe = Arc::new(Self {
            recipe: self.recipe.clone(),
            state: self.state.clone(),
            registry: self.registry.clone(),
        });
        Box::pin(recipe.construct(parent, options, context, None))
    }
    fn restore(
        &self,
        parent: AgentHandle,
        snapshot: ChildSnapshot,
        context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let recipe = Arc::new(Self {
            recipe: self.recipe.clone(),
            state: self.state.clone(),
            registry: self.registry.clone(),
        });
        let options = SpawnOptions::new().harness_model(snapshot.model());
        Box::pin(recipe.construct(parent, options, context, Some(snapshot)))
    }
    fn settings(&self, parent: AgentHandle) -> BackendFuture<Result<(HarnessModel, Thinking)>> {
        Box::pin(async move { parent.settings().await })
    }
}

fn error(error: impl std::fmt::Display) -> NanocodexError {
    NanocodexError::InvalidExecutionPolicy(error.to_string())
}

#[cfg(feature = "claude")]
impl DurableAgentExt for nanocodex_claude::ClaudeBuilder {
    async fn durability(self, state: DurableSession) -> Result<Self> {
        nanocodex_durability::DurableAgentExt::durability(self, state).await
    }
}
