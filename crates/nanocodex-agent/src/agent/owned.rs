//! Host ownership of an agent lifecycle and every lifecycle created from it.

use super::backend::{
    BackendFuture, BackendPrompt, BackendPromptRoute, BackendTurn, BackendTurnKey, LifecycleBackend,
};
use super::*;

/// Embedding-owned registry that retains agents until they are explicitly
/// shut down. Implemented by reusable runtimes; not a provider contract.
#[doc(hidden)]
pub trait AgentOwner: Send + Sync + 'static {
    /// Admits one construction, failing with [`NanocodexError::AgentStopped`]
    /// once the owner stopped admitting new agents.
    fn admit(&self) -> Result<()>;
    /// Ends one admitted construction, whether or not it produced an agent.
    fn finish(&self);
    /// Retains an owning handle under `key`, returning `false` after the
    /// owner already stopped every agent it held.
    fn adopt(&self, key: usize, agent: Nanocodex) -> bool;
    /// Drops the retained handle after the agent's explicit shutdown.
    fn release(&self, key: usize);
}

/// Places a freshly constructed agent under `owner`.
///
/// The returned handle shares the agent's lifecycle; agents created from it
/// through `spawn` or `fork` are owned as well. A shut-down agent is
/// released. The owner is held weakly, so a dropped owner leaves the agent
/// running under its remaining handles. An owner that already stopped its
/// agents rejects adoption: the agent is shut down and `AgentStopped` returned.
#[doc(hidden)]
pub async fn adopt(agent: Nanocodex, owner: &Arc<dyn AgentOwner>) -> Result<Nanocodex> {
    let key = Arc::as_ptr(&agent.backend).cast::<()>() as usize;
    let backend: Arc<dyn LifecycleBackend> = Arc::new(Owned {
        inner: Arc::clone(&agent.backend),
        owner: Arc::downgrade(owner),
        key,
    });
    let owned = Nanocodex {
        backend,
        events: agent.events.clone(),
        next_turn: Arc::clone(&agent.next_turn),
        agent_id: Arc::clone(&agent.agent_id),
        session: Arc::clone(&agent.session),
        #[cfg(feature = "openai")]
        local_session_id: agent.local_session_id,
    };
    if owner.adopt(key, owned.clone()) {
        return Ok(owned);
    }
    // The owner already stopped its agents: stop this one too, reporting a
    // failed cleanup rather than hiding it behind the rejection.
    agent.backend.shutdown().await?;
    Err(NanocodexError::AgentStopped)
}

/// Ends an admitted construction even when its future is dropped mid-await.
struct Admitted(Arc<dyn AgentOwner>);

impl Drop for Admitted {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Runs one admitted construction and adopts its result. The backend
/// operation starts only after admission succeeds.
async fn adopt_created(
    owner: std::sync::Weak<dyn AgentOwner>,
    create: impl FnOnce() -> BackendFuture<Result<(Nanocodex, AgentEvents)>>,
) -> Result<(Nanocodex, AgentEvents)> {
    let Some(owner) = owner.upgrade() else {
        return create().await;
    };
    owner.admit()?;
    let admitted = Admitted(Arc::clone(&owner));
    let (agent, events) = create().await?;
    let agent = adopt(agent, &owner).await?;
    drop(admitted);
    Ok((agent, events))
}

struct Owned {
    inner: Arc<dyn LifecycleBackend>,
    owner: std::sync::Weak<dyn AgentOwner>,
    key: usize,
}

impl LifecycleBackend for Owned {
    fn harness_family(&self) -> crate::HarnessFamily {
        self.inner.harness_family()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn persistence(&self) -> Option<Persistence> {
        self.inner.persistence()
    }
    fn submit(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendTurn>> {
        self.inner.submit(prompt)
    }
    fn route(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>> {
        self.inner.route(prompt)
    }
    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>> {
        self.inner.steer(key, prompt)
    }
    fn steer_with_id(
        &self,
        key: BackendTurnKey,
        id: String,
        prompt: Prompt,
    ) -> BackendFuture<Result<()>> {
        self.inner.steer_with_id(key, id, prompt)
    }
    fn withdraw_steer(&self, key: BackendTurnKey, id: String) -> BackendFuture<Result<bool>> {
        self.inner.withdraw_steer(key, id)
    }
    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>> {
        self.inner.cancel(key)
    }
    fn set_harness_model(&self, model: crate::HarnessModel) -> BackendFuture<Result<()>> {
        self.inner.set_harness_model(model)
    }
    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>> {
        self.inner.set_thinking(thinking)
    }
    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<Result<()>> {
        self.inner.set_fast_mode(enabled)
    }
    fn set_service_tier(&self, service_tier: ServiceTier) -> BackendFuture<Result<()>> {
        self.inner.set_service_tier(service_tier)
    }
    fn compact(&self) -> BackendFuture<Result<()>> {
        self.inner.compact()
    }
    fn append_developer_message(
        &self,
        text: String,
    ) -> BackendFuture<Result<crate::AgentSessionContext>> {
        self.inner.append_developer_message(text)
    }
    fn context(&self) -> BackendFuture<Result<crate::AgentSessionContext>> {
        self.inner.context()
    }
    fn checkpoint(&self) -> BackendFuture<Result<SessionCheckpoint>> {
        self.inner.checkpoint()
    }
    fn persist_initial(&self) -> BackendFuture<Result<()>> {
        self.inner.persist_initial()
    }
    fn discard_initial(&self) -> BackendFuture<Result<()>> {
        self.inner.discard_initial()
    }
    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(adopt_created(self.owner.clone(), move || {
            inner.spawn(options)
        }))
    }
    fn fork(&self, request: ForkRequest) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(adopt_created(self.owner.clone(), move || {
            inner.fork(request)
        }))
    }
    fn flush(&self) -> BackendFuture<Result<()>> {
        self.inner.flush()
    }
    fn disconnect(&self) -> BackendFuture<Result<()>> {
        let disconnected = self.inner.disconnect();
        let owner = self.owner.clone();
        let key = self.key;
        Box::pin(async move {
            let result = disconnected.await;
            if let Some(owner) = owner.upgrade() {
                owner.release(key);
            }
            result
        })
    }
    fn shutdown(&self) -> BackendFuture<Result<()>> {
        let stopped = self.inner.shutdown();
        let owner = self.owner.clone();
        let key = self.key;
        Box::pin(async move {
            let result = stopped.await;
            if let Some(owner) = owner.upgrade() {
                owner.release(key);
            }
            result
        })
    }
}
