# Nanocodex

The batteries-included façade for the Nanocodex frontier-agent building blocks.

This crate contains no second agent engine. It re-exports the owned agent
lifecycle ([`Agent`]) and adds [`Nanocodex`], a reusable runtime that
constructs many isolated agents from shared provider clients. Depending on
`nanocodex-agent` directly creates the same agents.

Upgrading from 0.5? Read the [Rust API changelog and migration guide](https://github.com/gakonst/nanocodex/blob/v0.6.0/docs/MIGRATING_0_6.md)
for breaking signatures, changed defaults, and snapshot/tool migrations.

## Quick start

Configure a runtime once, then spawn agents from it. Each agent owns its own
conversation, tool runtime and event stream; the clients and their
authentication are shared:

```rust,no_run
use nanocodex::{Nanocodex, OpenAi};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let nano = Nanocodex::builder()
    .openai(OpenAi::new(std::env::var("OPENAI_API_KEY")?)?)
    .instructions("You are a Rust coding agent. Preserve unrelated work and run relevant tests.")
    .workspace(std::env::current_dir()?)
    .build()?;

let (agent, _events) = nano.spawn().build().await?;
let result = agent
    .prompt("Explain the cause of the failing parser test.")
    .await?
    .await?;
println!("{}", result.final_message());
nano.shutdown().await?;
# Ok(())
# }
```

Awaiting `prompt` means the private driver accepted and ordered the turn.
Awaiting the returned [`Turn`] waits for its complete [`TurnResult`]; it does
not wait for the turn's optional event stream to be consumed. Follow-on prompts
reuse the same retained context and transport without asking the caller to
manage response IDs or history.

## Two harnesses, one runtime

Configure each harness with its own client, tools, Code Mode and workspace.
Runtime-level `instructions` and `workspace` are defaults for both; each
spawn may override them (spawn > harness > runtime):

```rust,no_run
# #[cfg(all(feature = "claude", feature = "openai"))]
# async fn mixed() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{
    ClaudeHarness, ClaudeModel, CodexHarness, HarnessFamily, Model, Nanocodex, OpenAi, Thinking,
    Tools, claude::ClaudeClient,
};

let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let claude = ClaudeClient::official(reqwest::Client::new(), std::env::var("ANTHROPIC_API_KEY")?);
let nano = Nanocodex::builder()
    .openai(CodexHarness::new(openai).tools(Tools::default()).code_mode(true))
    .claude(
        ClaudeHarness::new(claude)
            .tools(Tools::builder().without_defaults().build()?)
            .code_mode(false)
            .configure(|builder| Ok(builder.max_tokens(8_192))),
    )
    .harness(HarnessFamily::Codex)
    .model(Model::Sol)
    .thinking(Thinking::High)
    .build()?;

let (codex, _events) = nano.spawn().instructions("Review the diff.").build().await?;
let (sonnet, _events) = nano.spawn().model(ClaudeModel::Sonnet55).build().await?;
codex.prompt("Summarize the open issues.").await?.await?;
let saved = sonnet.checkpoint().await?;
sonnet.shutdown().await?;
let (_resumed, _events) = nano.resume(saved).await?;
nano.shutdown().await?;
# Ok(())
# }
```

Selection rules:

- Without a family or model, agents use Codex when it is configured and
  otherwise the sole configured family, its default model and that model's
  default thinking. `build()` fails without any backend.
- A model selects its own family. A family other than the runtime default uses
  that family's default model; a model other than the runtime default uses its
  own default thinking. These are the same rules children use against their
  parent.
- A model of another family, thinking the model does not support, or an
  unconfigured family fail with `NanocodexError::InvalidRequest` before any
  provider request. Thinking is never clamped.

Tools belong to each harness and are materialized separately for every agent.
Codex installs [`Tools`] natively and defaults to the standard Codex tools.
Claude defaults to no client tools; configured [`Tools`] are bridged into its native
client catalog. `code_mode(true)` exposes them through Code Mode's `exec` and `wait`;
`code_mode(false)` exposes each tool as a direct provider tool (freeform tools take
their raw text in `input`; deferred or asynchronous tools are rejected for Claude
direct mode). OpenAI-hosted web search and image generation have no local
handler and are not bridged. `workspace` fixes the agents' working directory and
context root; it does not install file or shell tools. `configure` applies native
[`CodexBuilder`] or Claude builder settings to every agent of that harness. The
Codex harness uses the standard Responses service of its client; hosts with a
custom Tower service factory build agents with `Agent::builder` and a [`Harness`].

### Lifecycle ownership

Agents the runtime constructs (roots, resumed sessions, and siblings or forks
created from them) stay owned by it until they are shut down, even after their
handles are dropped. `nano.shutdown()` stops admitting agents, waits for
constructions already admitted, shuts down every owned agent and then every
live subagent routed through its recipes, and shares one result across calls;
a cancelled call leaves the rest for the next. Subagents stay owned by their
registry, which may evict idle children. Dropping the last runtime clone
releases ownership without stopping agents whose handles are still held. An
agent built directly with `Agent::builder` keeps its own drop-to-clean-up semantics.

### Subagents across families

Install a shared `nanocodex-subagents` registry once per harness. Children route
through the runtime's recipes, so a Codex parent can start a Claude child and
the reverse:

```rust,no_run
# #[cfg(all(feature = "claude", feature = "openai"))]
# fn subagents(openai: nanocodex::OpenAi, claude: nanocodex::claude::ClaudeClient) -> nanocodex::agent::Result<nanocodex::Nanocodex> {
use std::sync::Arc;
use nanocodex::{ClaudeHarness, CodexHarness, Nanocodex, Tools};
use nanocodex_subagents::{channel, install_tools};

let (registry, _control, _updates) = channel(4);
let subagent_tools = move |handle| {
    install_tools(Tools::default(), handle, Arc::clone(&registry))
};
Nanocodex::builder()
    .openai(CodexHarness::new(openai).tools_factory(subagent_tools.clone()))
    .claude(ClaudeHarness::new(claude).tools_factory(subagent_tools))
    .build()
# }
```

A routed child uses its harness and runtime settings, not a sibling's per-spawn
overrides. Omitting child overrides inherits the live parent's family, model and
effort; selecting another family uses that family's defaults.

The [runtime journey](tests/it/runtime.rs) drives both real native builders
against localhost Responses HTTP and Messages SSE fixtures: backend errors, two
isolated roots with overrides, cross-family subagents, resume, direct tools,
and shutdown. Run
`cargo test -p nanocodex --all-features --test it runtime -- --nocapture`; the
provider request transcript is retained in ignored `output/library-runtime/`.

## One agent

The façade's former `Nanocodex` agent handle is now named `Agent`, and its
former `NanocodexBuilder` is `CodexBuilder`. Existing single-agent code changes
`Nanocodex::builder(client)` to `Agent::builder(client)`; `Nanocodex` now names
the configured runtime. The lower-level `nanocodex-agent` names are unchanged.

`Agent::builder` builds a single agent from one backend with its complete native
builder:

```rust,no_run
use nanocodex::{Agent, OpenAi};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Agent::builder(openai)
    .instructions("Answer concisely and preserve exact identifiers.")
    .build()?;

let result = agent.prompt("Explain the identifier req_7f3.").await?.await?;
if let Some(usage) = result.usage() {
    if let Some(cost) = usage.estimated_cost() {
        println!("estimated {}", cost.amount());
    } else {
        println!("cost unavailable: {}", usage.cost_status().as_str());
    }
}
agent.shutdown().await?;
# Ok(())
# }
```

When the provider reports aggregate usage for a completed turn, Nanocodex
applies the selected model's published standard or priority rates, including
long-context multipliers.

`gpt-6-astra` with low reasoning is the single-agent default; `.model(Model::Sol)`
and `.model(Model::Luna)` select the other supported models. Model selection
uses the catalog's default reasoning unless an effort was explicitly selected.
A caller may change the model before the first turn is accepted; it then remains
fixed for the thread.

## Claude

Enable `claude` on the facade to use Anthropic Messages with the same agent
lifecycle and durability extension:

```toml
[dependencies]
nanocodex = { version = "0.6.7", features = ["claude"] }
reqwest = "0.13"
```

```rust,no_run
# #[cfg(all(feature = "claude", feature = "durability"))]
# async fn claude_turn() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{Agent, Claude, DurableAgentExt};
use nanocodex::claude::ClaudeClient;
use nanocodex::durability::{DurableSession, MemoryStore};

let client = ClaudeClient::official(
    reqwest::Client::new(),
    std::env::var("ANTHROPIC_API_KEY")?,
);
let state = DurableSession::open(MemoryStore::new()?, "claude-session").await?;
let (agent, _events) = Agent::builder(Claude::latest(client))
    .system("Answer concisely.")
    .durability(state)
    .await?
    .build()?;
let result = agent.prompt("Explain durable request replay.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

`MemoryStore` retains state in memory; use a persistent host store when state
must survive process restarts. The facade automatically enables the Claude
adapter whenever `claude` and `durability` are enabled together.

Default features remain `durability`, `mcp`, `openai`, and `tools`. For the minimal
Claude provider path, use `default-features = false, features = ["claude"]`.
Add `durability` for the extension above; durability retains its existing
OpenAI dependency. Add `claude-tools` to expose Claude's optional tool adapters
through `nanocodex::claude_tools`. The separate `workspace-tools` feature enables
the OpenAI workspace runtime. The reusable [`Nanocodex`] runtime is available on native
targets; WebAssembly hosts use [`Agent`] and [`Harness`].

## Muse

Enable `muse` for the Muse-owned adaptation of the reference agent loop, sharing
Responses transports and tool types. `Muse::builder(auth)` selects Spark;
`.model(MuseModel::Wonder10)` and `.model(MuseModel::WonderFull10)` select Wonder.
Build one common `Agent` with `Agent::builder(muse)`, or share a provider across
agents with `Nanocodex::builder().muse(muse).build()?` and `nano.spawn().build().await?`.
Image generation remains opt-in through `Tools::builder().image_generation(true)`.
Muse owns its HTTP/SSE protocol, OAuth, images and summary compaction; the common
lifecycle and runtime come from upstream. See [nanocodex-muse](../nanocodex-muse/README.md).

## Host-composed harnesses

[`Harness`] composes explicitly registered construction recipes when a host
needs full control of each native builder; the runtime is built on the same
router. Each recipe keeps its concrete provider, service type, authentication,
tools and execution policy until `.build()` returns the common `(Agent, AgentEvents)`:

```rust,no_run
# #[cfg(all(feature = "claude", feature = "openai"))]
# async fn mixed() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{
    Agent, Claude, ClaudeModel, DurableAgentExt as _, Harness, HarnessFamily, HarnessModel,
    Model, OpenAi,
    agent::SpawnOptions,
    claude::ClaudeClient,
};

let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let claude = ClaudeClient::official(
    reqwest::Client::new(), std::env::var("ANTHROPIC_API_KEY")?,
);
let harness = Harness::builder()
    .register(HarnessFamily::Codex, move |request| {
        let openai = openai.clone();
        async move {
            let HarnessModel::Codex(model) = request.model else { unreachable!() };
            let mut builder = Agent::builder(openai)
                .model(model).thinking(request.thinking)
                .host_context(request.host_context)
                .spawn_factory(request.spawn_factory);
            if let Some(checkpoint) = request.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            if let Some(state) = request.durable_state {
                builder = builder.durability(state).await?;
            }
            builder.build()
        }
    })
    .register(HarnessFamily::Claude, move |request| {
        let claude = claude.clone();
        async move {
            let mut builder = Agent::builder(Claude::new(claude, request.model.as_str()))
                .thinking(request.thinking)?
                .host_context(request.host_context)
                .spawn_factory(request.spawn_factory);
            if let Some(checkpoint) = request.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            if let Some(state) = request.durable_state {
                builder = builder.durability(state).await?;
            }
            builder.build()
        }
    })
    .build();

let (codex, _events) = harness.start(HarnessModel::Codex(Model::Sol)).await?;
let (claude, _events) = harness.start_with(
    SpawnOptions::new().harness_model(HarnessModel::Claude(ClaudeModel::Sonnet55)),
).await?;
println!("{}", claude.prompt("Explain the parser.").await?.await?.final_message());
claude.shutdown().await?;
codex.shutdown().await?;
# Ok(())
# }
```

The same router reopens sessions of either family: `harness.resume(checkpoint)`
reopens a portable `SessionCheckpoint` and `harness.open(&store, id)` reopens a
session stored in a durable `SessionStore` catalog. Install host capabilities
through each concrete builder's `.tools_factory(...)`; Codex returns `Tools`,
Claude returns native `ClaudeTools`. `request.spawn_factory` must be attached to each
recipe so descendants route through the same harness. Idle residency checkpoints
restore through the recipe with newly authorized host tools and credentials;
they do not establish process-restart durability.

The [harness journey](tests/it/harness.rs) exercises shared registry completion,
idle restoration and stopped-owner fencing with
`cargo test -p nanocodex --all-features --test it harness:: -- --nocapture`.

## Progressive disclosure

The root exports only the golden-path types. Reach for a named module when an
embedding needs more control:

- [`agent`] — lifecycle policy, events, input, sessions, usage, and rollout
- [`durability`] — optional durable admission, effect replay, checkpoints, and
  host-store contracts layered over an agent
- [`oai`] — managed Responses sessions and the concrete Tower boundary
- `claude` — Anthropic Messages client, builder, protocol, and authentication
  when the default-off `claude` feature is enabled
- [`tools`] — tool contracts, built-ins, Code Mode, and MCP
- `observability` — native tracing and OTLP setup when the default-off
  `observability` feature is enabled
- [`prelude`] — common imports for the runtime and agent path

Detailed items retain the documentation from their owning crate. Each lower
crate also includes its own focused guide and can be documented or consumed
without the facade.

## Canonical imports

Use the crate root for the common path and the module that owns a concept when
reaching for its detailed API:

```rust
use nanocodex::{Agent, Nanocodex, OpenAi};
use nanocodex::agent::{events::AgentEvent, session::SessionSnapshot};
use nanocodex::durability::{DurableSession, MemoryStore};
use nanocodex::oai::tower::ResponsesAttempt;
use nanocodex::tools::mcp::Mcp;

# fn type_check(
#     _: Option<Agent>,
#     _: Option<Nanocodex>,
#     _: Option<OpenAi>,
#     _: Option<AgentEvent>,
#     _: Option<SessionSnapshot>,
#     _: Option<DurableSession>,
#     _: Option<MemoryStore>,
#     _: Option<ResponsesAttempt>,
#     _: Option<Mcp>,
# ) {}
```

The root convenience path and its owning module name the same type; for
example, [`Agent`] and [`agent::Agent`] are identical, as are [`OpenAi`] and
[`oai::OpenAi`]. Applications that need only one component can depend on its
package directly and use `nanocodex_oai_api`, `nanocodex_oai_tools`,
`nanocodex_agent`, or `nanocodex_durability`.
