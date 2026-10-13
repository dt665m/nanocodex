use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "openai")]
pub(super) struct TurnKey(pub(super) u64);

#[cfg(feature = "openai")]
pub(super) enum Command {
    Prompt {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<ExecutionOperation>,
        accepted: Option<oneshot::Sender<Result<String>>>,
        cancel_on_admission: bool,
        thinking: Option<Thinking>,
        service_tier: Option<ServiceTier>,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    SteerWithId {
        key: TurnKey,
        id: String,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    WithdrawSteer {
        key: TurnKey,
        id: String,
        result: oneshot::Sender<Result<bool>>,
    },
    RoutePrompt {
        key: TurnKey,
        prompt: Prompt,
        parent: Option<tracing::Span>,
        events: EventSink,
        turn_result: oneshot::Sender<Result<TurnResult>>,
        route_result: oneshot::Sender<Result<PromptRouteKind>>,
    },
    Cancel {
        key: TurnKey,
        result: oneshot::Sender<Result<()>>,
    },
    Fork {
        origin: crate::Origin,
        point: ForkFrom,
        session_id: SessionId,
        policy: Option<Arc<dyn execution::ExecutionPolicy>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    /// Captures this driver's identity and latest committed boundary without
    /// waiting for an active turn.
    ChildSnapshot {
        result: oneshot::Sender<Result<ChildState>>,
    },
    Spawn {
        restore: Option<ChildState>,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    SpawnBatch {
        count: usize,
        observer: Option<Arc<SpawnObserver>>,
        host_context: Option<Arc<str>>,
        result: oneshot::Sender<Result<Vec<(Nanocodex, AgentEvents)>>>,
    },
    SetModel {
        model: Model,
        result: oneshot::Sender<Result<()>>,
    },
    SetThinking {
        thinking: Thinking,
        result: oneshot::Sender<Result<()>>,
    },
    SetServiceTier {
        service_tier: ServiceTier,
        result: oneshot::Sender<Result<()>>,
    },
    Compact {
        parent: Option<tracing::Span>,
        result: oneshot::Sender<Result<()>>,
    },
    AppendDeveloperMessage {
        text: String,
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Context {
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Shutdown,
}

/// Driver-resolved boundary a fork starts from.
#[cfg(feature = "openai")]
pub(super) enum ForkFrom {
    /// The latest committed boundary held by the driver.
    Latest,
    /// A live boundary retained by a completed turn of this conversation.
    Live(Arc<CommittedSession>),
    /// A decoded portable checkpoint of this conversation tree.
    Snapshot(Box<SessionSnapshot>),
}

#[cfg(feature = "openai")]
#[derive(Clone)]
pub(super) enum ExecutionOperation {
    Caller(String),
    Automatic(String),
    Admitted(String),
    Recovered(String),
}

#[cfg(feature = "openai")]
impl ExecutionOperation {
    pub(super) fn into_id(self) -> String {
        match self {
            Self::Caller(operation_id)
            | Self::Automatic(operation_id)
            | Self::Admitted(operation_id)
            | Self::Recovered(operation_id) => operation_id,
        }
    }

    pub(super) fn id(&self) -> &str {
        match self {
            Self::Caller(operation_id)
            | Self::Automatic(operation_id)
            | Self::Admitted(operation_id)
            | Self::Recovered(operation_id) => operation_id,
        }
    }

    pub(super) const fn is_recovered(&self) -> bool {
        matches!(self, Self::Recovered(_))
    }
}

#[cfg(feature = "openai")]
pub(super) enum PromptRouteKind {
    Started { request_id: Option<String> },
    Steered,
}

#[cfg(feature = "openai")]
pub(super) enum QueuedTurn {
    Pending {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<ExecutionOperation>,
        thinking: Thinking,
        service_tier: ServiceTier,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Cancelled {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<ExecutionOperation>,
        cancellation_committed: bool,
        thinking: Thinking,
        service_tier: ServiceTier,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}
