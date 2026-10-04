# xAI backend

`nanocodex-xai` adapts the open-source Grok Build Responses conversation path
into the common `nanocodex-agent` lifecycle. The public Rust facade exposes it
with the default-off `xai` feature, `nanocodex::Xai`, and
`nanocodex::xai`. Construction follows the same provider-specific builder pattern
as Nanoclaude. The implementation runs in process and does not require an
installed `grok` executable.

## Construction

From this checkout, an application can depend on the facade with
`default-features = false, features = ["xai"]`. It also needs `reqwest = "0.13"`
when constructing the HTTP client explicitly.

```rust,no_run
use nanocodex::{Nanocodex, Xai, XaiModel};
use nanocodex::xai::XaiClient;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = XaiClient::new(
    reqwest::Client::new(),
    "https://api.x.ai/v1/responses",
    std::env::var("XAI_API_KEY")?,
);
let (agent, _events) = Nanocodex::builder(Xai::new(
    client,
    XaiModel::Grok46.as_str(),
))
.build()?;
let first = agent.prompt("Remember the word violet.").await?.await?;
let next = agent.prompt("Which word did I give you?").await?.await?;
println!("{}\n{}", first.final_message(), next.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

The embedding supplies its API key and HTTP client. The backend does not read
Grok CLI credential files or perform browser login. The endpoint is explicit,
which also permits host-owned proxies and local protocol fixtures. Model
availability and account access remain provider decisions; selecting an entry
in `XaiModel` does not establish either.

`HarnessFamily::Xai` and `HarnessModel::Xai(XaiModel::Grok46)` identify this
backend in the shared model catalog. The facade reexports `XaiModel` even when
the provider feature is disabled, just as it does `ClaudeModel`. Enabling `xai`
does not enable the existing OpenAI runtime, workspace tools, or durability
extension. The facade's default features are unchanged.

## Delivered boundary

The backend owns its Responses HTTP/SSE transport and in-memory conversation.
Follow-on turns replay native conversation items, including reasoning and
function-call/result associations. Requests use `store: false`. A completed
response is required before the tool loop acts on it; a truncated or failed
response is an error. There are no automatic provider retries.

The builder exposes `.system(...)`, `.thinking(...)`, `.max_steps(...)`, and
`.request_timeout(...)`. The defaults are 32 model calls per turn and a
300-second timeout per sampling call. Prompts currently accept text. A
`PromptRequest` with a durable request ID is rejected because this backend has
no durable deduplication store.

Host function handlers are explicit capabilities, registered with
`.tool(ToolDefinition { name, description, parameters }, callback)`. The callback
receives a JSON value and asynchronously returns `Result<String, String>`.
`.web_search()` requests an xAI-hosted tool; it does not execute a local callback.
Cancellation stops sampling and skips pending callbacks. A callback that has
already started must settle and have its receipt recorded before cancellation
is acknowledged or shutdown releases the session. Its host must bound any
external work it starts.

The upstream CLI's shell,
filesystem, browser, MCP, and subagent tools are not installed automatically.
The common lifecycle exposes prompt results, events, cancellation and shutdown.
This adaptation does not import the full Grok Build application, its TUI, ACP
server, CLI configuration discovery, subscription authentication, sandbox,
compaction/recovery policies, or persisted session database. No shared durable
store adapter or process-restart replay guarantee is supplied by `xai`.

Live steering, forking, transcript conversion through `.context()`, developer
message injection, and automatic/manual compaction return explicit unsupported
errors. Clean same-family children can be constructed with the shared lifecycle;
this does not provide mixed-family child routing or idle checkpoint restoration.
Long conversations retain their full history and require an application-owned
session policy.

The portable library boundary is distinct from a deployed product integration.
A Rust feature does not add JavaScript exports, a Worker provider, product model
selection, sign-in UI, or permission to call an account. Full Grok Build parity
and live provider acceptance are not implied by local protocol tests.

## Upstream provenance and licenses

The source reference is the official
[`xai-org/grok-build`](https://github.com/xai-org/grok-build/tree/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8)
repository at commit `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`.
Its `SOURCE_REV` records monorepo revision
`559751fdcec02d413e4c57c8832ab275e4f44980`. These identify the inspected source;
they are not a floating Cargo dependency or a claim that the entire upstream
workspace was copied.

Relevant upstream boundaries are
[`xai-grok-sampling-types/src/conversation/responses.rs`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs)
for conversation conversion and
[`xai-grok-sampler/src/stream/responses.rs`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-sampler/src/stream/responses.rs)
for streamed Responses semantics. The upstream sampling/tool loop is in
`xai-grok-shell/src/session/acp_session_impl/turn.rs`
(`process_conversation_turn_inner`) and `tool_calls.rs` in the same directory.
`xai-grok-shell/src/agent/mvp_agent/` handles host setup; `xai-grok-agent` alone
is an agent-definition and prompt builder.

Upstream first-party source is Copyright 2023–2026 SpaceXAI, licensed under
Apache-2.0. The backend's [provenance notice](../crates/nanocodex-xai/UPSTREAM.md)
lists the exact adapted files and modifications; its
[retained license](../crates/nanocodex-xai/THIRD-PARTY-LICENSES) contains the
upstream copyright and complete Apache license. Upstream third-party and
vendored code retains its own licenses, as recorded in its
[`THIRD-PARTY-NOTICES`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/THIRD-PARTY-NOTICES).
The upstream Apache license does not replace those separate licenses.

## Validation

Run the public facade journey without default provider features:

```sh
cargo test -p nanocodex --no-default-features --features xai --test it xai:: -- --nocapture
```

It uses the public facade builder and an actual loopback HTTP/SSE server to
check two successive prompts, retained history, provider rejection and shutdown
fencing. Its synthetic request transcript is written to ignored
`output/xai/facade-requests.json`. Backend protocol journeys run with
`cargo test -p nanocodex-xai -- --nocapture`.
These fixtures replace only the external model provider; they do not establish
successful authentication or inference against the live xAI service.
