#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

/// One owned agent lifecycle: prompts, turns, steering, settings, forks and
/// checkpoints, the same for every harness family.
///
/// Build one directly with `Agent::builder(OpenAi)` or
/// `Agent::builder(Claude)`, or construct many from a reusable
/// [`Nanocodex`] runtime.
pub use nanocodex_agent::Nanocodex as Agent;
/// The single-backend Codex builder returned by `Agent::builder(openai)`.
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_agent::NanocodexBuilder as CodexBuilder;
pub use nanocodex_agent::{
    AgentEvents, AgentSessionContext, Capabilities, ClaudeModel, CostStatus, EstimatedUsdCost,
    ExecutionPolicyDisposition, ForkPoint, ForkRequest, HarnessFamily, HarnessModel, Lineage,
    ModelCapabilities, ModelTransport, Mutability, NanocodexError, Origin, Persistence,
    PromptRequest, PromptRoute, ReportedTurnUsage, ServiceTier, SessionCheckpoint, SessionInfo,
    Turn, TurnControl, TurnResult, TurnUsage, UsdAmount, WeakAgent,
};
mod harness;
pub use harness::{Harness, HarnessBuilder, HarnessRequest};
#[cfg(not(target_family = "wasm"))]
mod runtime;
#[cfg(feature = "claude")]
#[cfg_attr(docsrs, doc(cfg(feature = "claude")))]
pub use nanocodex_claude::Claude;
#[cfg(feature = "muse")]
#[cfg_attr(docsrs, doc(cfg(feature = "muse")))]
pub use nanocodex_muse::{Muse, MuseBuilder, MuseModel};

#[cfg(feature = "durability")]
#[cfg_attr(docsrs, doc(cfg(feature = "durability")))]
pub use nanocodex_durability::DurableAgentExt;
#[cfg(all(not(target_family = "wasm"), feature = "managed"))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(not(target_family = "wasm"), feature = "managed")))
)]
pub use nanocodex_managed::{Managed, ManagedApiKey};
/// Muse Responses provider and caller-owned native OAuth credentials.
#[cfg(feature = "muse")]
#[cfg_attr(docsrs, doc(cfg(feature = "muse")))]
#[doc(inline)]
pub use nanocodex_muse as muse;
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_oai_api::OpenAi;
pub use nanocodex_oai_api::{Model, ReasoningMode, Thinking};
#[cfg(feature = "oai-tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
pub use nanocodex_oai_tools::Tool;
#[cfg(all(
    feature = "oai-tools",
    any(feature = "openai", not(target_family = "wasm"))
))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(
        feature = "oai-tools",
        any(feature = "openai", not(target_family = "wasm"))
    )))
)]
pub use nanocodex_oai_tools::Tools;
#[cfg(all(feature = "openai", feature = "oai-tools", not(target_family = "wasm")))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(feature = "openai", feature = "oai-tools", not(target_family = "wasm"))))
)]
pub use nanocodex_oai_tools::tool;
#[cfg(all(not(target_family = "wasm"), feature = "claude"))]
#[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "claude"))))]
pub use runtime::ClaudeHarness;
#[cfg(all(not(target_family = "wasm"), feature = "openai"))]
#[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "openai"))))]
pub use runtime::CodexHarness;
#[cfg(all(not(target_family = "wasm"), feature = "muse"))]
#[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "muse"))))]
pub use runtime::MuseHarness;
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub use runtime::{Nanocodex, NanocodexBuilder, SpawnBuilder};

/// Owned agent lifecycle, builders, turns, branching, and checkpoints.
///
/// Provider and tool-runtime APIs keep their canonical detailed paths under
/// [`crate::oai`] and [`crate::tools`].
pub mod agent {
    #[cfg(feature = "durability")]
    #[cfg_attr(docsrs, doc(cfg(feature = "durability")))]
    pub use crate::durability;
    pub use nanocodex_agent::Nanocodex as Agent;
    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
    pub use nanocodex_agent::rollout;
    pub use nanocodex_agent::{
        AgentEvents, AgentSessionContext, BuilderBackend, CostStatus, EstimatedUsdCost,
        ExecutionPolicyDisposition, NanocodexError, PromptRequest, PromptRoute, ReportedTurnUsage,
        Result, ServiceTier, SpawnOptions, Turn, TurnControl, TurnResult, TurnUsage, UsdAmount,
        WeakAgent, events, input, session, usage,
    };
    pub use nanocodex_agent::{
        AgentHandle, Capabilities, ForkPoint, ForkRequest, Lineage, Mutability, Origin,
        Persistence, SessionCheckpoint, SessionInfo,
    };
    #[cfg(feature = "openai")]
    #[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
    pub use nanocodex_agent::{ExecutionEnvironment, NanocodexBuilder as CodexBuilder, execution};
}

/// Anthropic Messages client, provider-native builder, protocol, and authentication.
#[cfg(feature = "claude")]
#[cfg_attr(docsrs, doc(cfg(feature = "claude")))]
#[doc(inline)]
pub use nanocodex_claude as claude;

/// Portable durable execution policy and host-store contracts.
#[cfg(feature = "durability")]
#[cfg_attr(docsrs, doc(cfg(feature = "durability")))]
#[doc(inline)]
pub use nanocodex_durability as durability;

/// Tower-native OpenAI Responses client, sessions, protocol, and transport.
#[doc(inline)]
pub use nanocodex_oai_api as oai;

/// OpenAI Responses tool registry, built-ins, MCP, tool search, and Code Mode.
///
/// Compatibility facade for [`crate::oai_tools`]; Claude adapters live under
/// `nanocodex::claude_tools` with the `claude-tools` feature.
#[cfg(feature = "oai-tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
#[doc(inline)]
pub use nanocodex_oai_tools as tools;

/// OpenAI-specific tool contracts, catalog and execution runtime.
#[cfg(feature = "oai-tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
#[doc(inline)]
pub use nanocodex_oai_tools as oai_tools;

/// Claude-native tool adapters and host capability contracts.
#[cfg(feature = "claude-tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "claude-tools")))]
#[doc(inline)]
pub use nanocodex_claude_tools as claude_tools;

/// Native account-managed backend, administration client, and durable event transport.
#[cfg(all(not(target_family = "wasm"), feature = "managed"))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(not(target_family = "wasm"), feature = "managed")))
)]
#[doc(inline)]
pub use nanocodex_managed as managed;

/// Application-owned tracing and OpenTelemetry setup.
#[cfg(all(not(target_family = "wasm"), feature = "observability"))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(not(target_family = "wasm"), feature = "observability")))
)]
#[doc(inline)]
pub use nanocodex_observability as observability;

/// Common imports for the golden owned-agent path.
pub mod prelude {
    #[cfg(feature = "claude")]
    #[cfg_attr(docsrs, doc(cfg(feature = "claude")))]
    pub use crate::Claude;
    #[cfg(all(not(target_family = "wasm"), feature = "claude"))]
    #[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "claude"))))]
    pub use crate::ClaudeHarness;
    #[cfg(all(not(target_family = "wasm"), feature = "openai"))]
    #[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "openai"))))]
    pub use crate::CodexHarness;
    #[cfg(feature = "durability")]
    #[cfg_attr(docsrs, doc(cfg(feature = "durability")))]
    pub use crate::DurableAgentExt;
    #[cfg(all(not(target_family = "wasm"), feature = "muse"))]
    #[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "muse"))))]
    pub use crate::MuseHarness;
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    pub use crate::Nanocodex;
    #[cfg(feature = "oai-tools")]
    #[cfg_attr(docsrs, doc(cfg(feature = "oai-tools")))]
    pub use crate::Tool;
    #[cfg(all(
        feature = "oai-tools",
        any(feature = "openai", not(target_family = "wasm"))
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            feature = "oai-tools",
            any(feature = "openai", not(target_family = "wasm"))
        )))
    )]
    pub use crate::Tools;
    #[cfg(all(feature = "openai", feature = "oai-tools", not(target_family = "wasm")))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "openai", feature = "oai-tools", not(target_family = "wasm"))))
    )]
    pub use crate::tool;
    pub use crate::{Agent, ClaudeModel, HarnessFamily, HarnessModel, Model, Thinking};
    #[cfg(feature = "openai")]
    #[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
    pub use crate::{CodexBuilder, OpenAi};
    #[cfg(feature = "muse")]
    #[cfg_attr(docsrs, doc(cfg(feature = "muse")))]
    pub use crate::{Muse, MuseModel};
}

#[cfg(all(feature = "openai", feature = "oai-tools", not(target_family = "wasm")))]
#[doc(hidden)]
pub mod __private {
    pub use nanocodex_oai_tools::__private::*;
}
