#[cfg(feature = "openai")]
use super::handle::request_fork;
use super::*;

pub use nanocodex_agent_reference::backend::{
    AgentFactory, BackendFuture, BackendPrompt, BackendPromptRoute, BackendRuntime, BackendTurn,
    BackendTurnKey, ChildJournal, LifecycleBackend,
};

/// Resolves a public fork point against this Codex conversation tree.
#[cfg(feature = "openai")]
pub(super) fn resolve_fork_point(
    point: crate::session::ForkPoint,
    conversation_id: &str,
) -> Result<ForkFrom> {
    match point {
        crate::session::ForkPoint::Latest => Ok(ForkFrom::Latest),
        crate::session::ForkPoint::Turn(completed) => {
            let boundary = completed
                .boundary()
                .ok_or(NanocodexError::ReplayedCheckpointUnavailable)?;
            let Some(committed) = boundary.downcast::<CommittedSession>() else {
                // Only another family's boundary needs materializing to be identified.
                if !boundary.is::<super::checkpoint::ReplayedBoundary>()
                    && let Ok(checkpoint) = boundary.checkpoint()
                {
                    checkpoint.require_family(crate::HarnessFamily::Muse)?;
                }
                return Err(NanocodexError::ReplayedCheckpointUnavailable);
            };
            if committed.lineage_id() != conversation_id {
                return Err(NanocodexError::CheckpointLineageMismatch);
            }
            Ok(ForkFrom::Live(committed))
        }
        crate::session::ForkPoint::Checkpoint(checkpoint) => {
            checkpoint.require_family(crate::HarnessFamily::Muse)?;
            if checkpoint.conversation_id() != conversation_id {
                return Err(NanocodexError::CheckpointLineageMismatch);
            }
            let conversation = ChildState::from_checkpoint(checkpoint)?
                .conversation
                .ok_or(NanocodexError::ForkBeforeCompletedTurn)?;
            Ok(ForkFrom::Snapshot(Box::new(conversation)))
        }
        _ => Err(NanocodexError::UnsupportedCapability {
            capability: "fork_point",
        }),
    }
}

#[cfg(feature = "openai")]
pub(super) struct LocalLifecycle {
    pub(super) child_handle: AgentHandle,
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) execution: Execution,
    pub(super) shutdown: DriverShutdown,
    pub(super) checkpoints: Arc<CheckpointSource>,
    /// Resolves once the driver persisted a durable child's first checkpoint.
    pub(super) initial_ready: Arc<std::sync::Mutex<Option<oneshot::Receiver<Result<()>>>>>,
}

/// Lifecycle operations supported by the local Codex driver.
#[cfg(feature = "openai")]
const CODEX_CAPABILITIES: Capabilities = {
    let mut capabilities = Capabilities::NONE;
    capabilities.checkpoint = true;
    capabilities.resume = true;
    capabilities.fork = true;
    capabilities.fork_at = true;
    capabilities.side_conversation = true;
    capabilities.spawn = true;
    capabilities.steering = true;
    capabilities.identified_steering = true;
    capabilities.compaction = true;
    capabilities.developer_messages = true;
    capabilities.context = true;
    capabilities.model = crate::session::Mutability::BeforeFirstPrompt;
    capabilities.thinking = crate::session::Mutability::Anytime;
    capabilities.service_tier = crate::session::Mutability::Anytime;
    capabilities.ultrafast_service_tier = true;
    capabilities
};

#[cfg(feature = "openai")]
impl LifecycleBackend for LocalLifecycle {
    fn harness_family(&self) -> crate::HarnessFamily {
        crate::HarnessFamily::Muse
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = CODEX_CAPABILITIES;
        // Processing tiers are offered per model by the shared capability source.
        let model = self
            .child_handle
            .harness_model()
            .capabilities(crate::ModelTransport::Native);
        capabilities.ultrafast_service_tier = model.supports_service_tier(ServiceTier::Ultrafast);
        if !model.fast_mode() {
            capabilities.service_tier = crate::session::Mutability::Fixed;
        }
        capabilities
    }

    fn persistence(&self) -> Option<Persistence> {
        let durable = self.execution.durable_state_id().map(Persistence::durable);
        #[cfg(not(target_family = "wasm"))]
        if let Some(rollout) = self.execution.info().cloned() {
            return Some(durable.unwrap_or_default().with_rollout(rollout));
        }
        durable
    }

    fn submit(&self, request: BackendPrompt) -> BackendFuture<Result<BackendTurn>> {
        let commands = self.commands.clone();
        let execution = self.execution.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let execution_operation =
                request
                    .request_id
                    .map(ExecutionOperation::Caller)
                    .or_else(|| {
                        execution
                            .identifies_prompts()
                            .then(|| ExecutionOperation::Automatic(SessionId::new().to_string()))
                    });
            let (result, receiver) = oneshot::channel();
            let (accepted, acceptance) = if execution_operation.is_some() {
                let (accepted, acceptance) = oneshot::channel();
                (Some(accepted), Some(acceptance))
            } else {
                (None, None)
            };
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            if commands
                .send(Command::Prompt {
                    key: TurnKey(request.key.0),
                    prompt: request.prompt,
                    execution_operation,
                    accepted,
                    cancel_on_admission: request.cancel_on_admission,
                    thinking: None,
                    service_tier: None,
                    parent,
                    events: EventSink::from_publisher(request.events),
                    result,
                })
                .await
                .is_err()
            {
                return Err(shutdown.stopped_error().await);
            }
            let request_id = if let Some(acceptance) = acceptance {
                Some(match acceptance.await {
                    Ok(Ok(request_id)) => request_id,
                    Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                        return Err(shutdown.stopped_error().await);
                    }
                    Ok(Err(error)) => return Err(error),
                })
            } else {
                None
            };
            Ok(BackendTurn {
                request_id,
                result: Box::pin(async move {
                    receiver.await.map_err(|_| NanocodexError::TurnStopped)?
                }),
            })
        })
    }

    fn route(&self, request: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            let (turn_result, turn_receiver) = oneshot::channel();
            let (route_result, route_receiver) = oneshot::channel();
            if commands
                .send(Command::RoutePrompt {
                    key: TurnKey(request.key.0),
                    prompt: request.prompt,
                    parent,
                    events: EventSink::from_publisher(request.events),
                    turn_result,
                    route_result,
                })
                .await
                .is_err()
            {
                return Err(shutdown.stopped_error().await);
            }
            match route_receiver.await {
                Ok(Ok(PromptRouteKind::Started { request_id })) => {
                    Ok(BackendPromptRoute::Started(BackendTurn {
                        request_id,
                        result: Box::pin(async move {
                            turn_receiver
                                .await
                                .map_err(|_| NanocodexError::TurnStopped)?
                        }),
                    }))
                }
                Ok(Ok(PromptRouteKind::Steered)) => Ok(BackendPromptRoute::Steered),
                Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                    Err(shutdown.stopped_error().await)
                }
                Ok(Err(error)) => Err(error),
            }
        })
    }

    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Steer {
                key: TurnKey(key.0),
                prompt,
                result,
            })
            .await
        })
    }

    fn steer_with_id(
        &self,
        key: BackendTurnKey,
        id: String,
        prompt: Prompt,
    ) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SteerWithId {
                key: TurnKey(key.0),
                id,
                prompt,
                result,
            })
            .await
        })
    }

    fn withdraw_steer(&self, key: BackendTurnKey, id: String) -> BackendFuture<Result<bool>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::WithdrawSteer {
                key: TurnKey(key.0),
                id,
                result,
            })
            .await
        })
    }

    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Cancel {
                key: TurnKey(key.0),
                result,
            })
            .await
        })
    }

    fn set_harness_model(&self, model: crate::HarnessModel) -> BackendFuture<Result<()>> {
        let crate::HarnessModel::Muse(model) = model else {
            return Box::pin(async {
                Err(NanocodexError::InvalidRequest(
                    "model belongs to another harness family".into(),
                ))
            });
        };
        let model = model.into();
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetModel {
                model,
                result,
            })
            .await
        })
    }

    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetThinking {
                thinking,
                result,
            })
            .await
        })
    }

    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<Result<()>> {
        self.set_service_tier(ServiceTier::from_fast_mode(enabled))
    }

    fn set_service_tier(&self, service_tier: ServiceTier) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetServiceTier {
                service_tier,
                result,
            })
            .await
        })
    }

    fn compact(&self) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            request_command(&commands, &shutdown, |result| Command::Compact {
                parent,
                result,
            })
            .await
        })
    }

    fn append_developer_message(&self, text: String) -> BackendFuture<Result<AgentSessionContext>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| {
                Command::AppendDeveloperMessage { text, result }
            })
            .await
        })
    }

    fn context(&self) -> BackendFuture<Result<AgentSessionContext>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Context { result }).await
        })
    }

    fn checkpoint(&self) -> BackendFuture<Result<SessionCheckpoint>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::ChildSnapshot {
                result,
            })
            .await?
            .into_checkpoint()
        })
    }

    fn persist_initial(&self) -> BackendFuture<Result<()>> {
        let ready = self
            .initial_ready
            .lock()
            .ok()
            .and_then(|mut ready| ready.take());
        Box::pin(async move {
            match ready {
                Some(ready) => ready.await.map_err(|_| NanocodexError::AgentStopped)?,
                None => Ok(()),
            }
        })
    }

    fn discard_initial(&self) -> BackendFuture<Result<()>> {
        let execution = self.execution.clone();
        Box::pin(async move { execution.discard_initial_checkpoint().await })
    }

    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let parent = self.child_handle.clone();
        Box::pin(async move { parent.spawn_with(options).await })
    }

    fn fork(&self, request: ForkRequest) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        let checkpoints = Arc::clone(&self.checkpoints);
        let execution = self.execution.clone();
        Box::pin(async move {
            let (point, origin) = request.into_parts();
            let point = resolve_fork_point(point, checkpoints.conversation_id())?;
            let session_id = SessionId::new();
            let policy = execution.branch_policy(&checkpoints.child_info(&session_id, origin))?;
            request_fork(&commands, &shutdown, point, origin, session_id, policy).await
        })
    }

    fn flush(&self) -> BackendFuture<Result<()>> {
        #[cfg(not(target_family = "wasm"))]
        {
            let execution = self.execution.clone();
            Box::pin(async move { execution.flush().await })
        }
        #[cfg(target_family = "wasm")]
        {
            Box::pin(async { Ok(()) })
        }
    }

    fn shutdown(&self) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let execution = self.execution.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let (initiate, receiver) = shutdown.request();
            if initiate && commands.send(Command::Shutdown).await.is_err() {
                let outcome = match execution.shutdown().await {
                    Ok(()) => Err(NanocodexError::AgentStopped),
                    Err(error) => Err(error),
                };
                shutdown.complete(outcome);
            }
            match receiver.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(NanocodexError::Shutdown(error)),
                Err(_) => Err(NanocodexError::AgentStopped),
            }
        })
    }
}
