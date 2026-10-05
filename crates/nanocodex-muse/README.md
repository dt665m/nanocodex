# Nanocodex Muse

Muse Spark 1.3 over HTTP/SSE Responses, using Nanocodex's existing agent lifecycle,
Responses client, tools, sessions, snapshots, cancellation, and public types.
This crate adds the Muse provider recipe and native account authentication; it
re-exports `nanocodex-agent` rather than copying its implementation.

The `nanocodex` facade's optional `muse` feature also exports `Muse`,
`MuseBuilder`, and `nanocodex::muse::auth` (native targets), so applications can
use one dependency and the canonical Nanocodex types.

`Muse::builder(auth)` selects Standard (`muse-spark-1.3`),
`https://api.meta.ai/v1`, low reasoning, and client-owned full history (`store:
false`, encrypted reasoning replay). `.model(Model::MuseContributor)` selects
`muse-spark-1.3-contributor`, which permits Meta training. Contributor does not
support `Thinking::Max`. No WebSocket connection is attempted.

```rust,no_run
use nanocodex_muse::{Muse, Nanocodex};
# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let provider = Muse::builder(std::env::var("META_MODEL_API_KEY")?).build()?;
let (agent, _events) = Nanocodex::builder(provider)
    .workspace(std::env::current_dir()?)
    .build()?;
let result = agent.prompt("Say hello.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

## Authentication

Native `auth::MuseLogin::start()` exposes `verification_url()` and `user_code()`;
`complete()` performs device OAuth and `/muse-code/key` exchange. It returns a
`MuseCredential` with public `access_token: Option<String>` and `api_key: String`.
`exchange_muse_key()` returns the same pair. `import_muse_code_auth()` reads an
existing Muse Code login from macOS Keychain or fallback files without writing.

`MuseAuth::new(credentials)` keeps both credentials in memory and exposes the
shared `OpenAiAuth` through `authorization()`. Responses and image requests use
bounded 401 recovery: one exchange and one retry, serialized across concurrent
requests, with a generation check for late rejections. Failed exchange retains
previous credentials; OAuth 401/403 requires a new login and transient failures
honor Retry-After or a 30-second cooldown. No undocumented expiry or refresh-token
behavior is assumed. Debug output and errors omit tokens and response bodies.

The library never persists credentials. Hosts retain the manager and call
`credentials().await` to retrieve the current pair for their own storage.
Credentials never enter prompts, session snapshots, or agent tracing.

```rust,no_run
use nanocodex_muse::{Muse, auth::{MuseAuth, MuseLogin}};
# async fn login() -> Result<(), Box<dyn std::error::Error>> {
let login = MuseLogin::start().await?;
println!("Open {} and enter {}", login.verification_url(), login.user_code());
let manager = MuseAuth::new(login.complete().await?)?;
let provider = Muse::builder(manager.authorization()).build()?;
let credentials = manager.credentials().await; // Persistence is the host's choice.
# let _ = (provider, credentials);
# Ok(())
# }
```

OAuth parameters follow the Muse CLI and the
[OpenCode device-auth plugin](https://github.com/TheStreamCode/opencode-muse-auth/blob/8ff829b4e18a5600f25e82ee186af40ff4ea2d47/src/auth.ts);
credential lookup follows
[opencode-muse-code](https://github.com/swalker326/opencode-muse-code/blob/4690c3f4540343c0138224d2c5a9caa7c8b6b0a0/index.ts).

## Context and images

The shared agent uses Claude-style client-side summary compaction for Muse:
reserve 20,000 output plus 13,000 thinking tokens, summarize through Responses
with tools disabled, validate a completed nonempty summary, then retain the
latest complete reasoning/tool round and receipts. Manual compaction, automatic
compaction, and context-window recovery use the same path. Repeated compaction
at one boundary is suppressed and rapid compactions are throttled. Other model
providers retain their existing compaction behavior.

`input::Prompt::content` accepts upstream `UserInput::LocalImage`, `Image` (data
or public HTTP(S) URL), and `ImageFile`. The shared image machinery preserves
`auto`, `low`, `high`, and `original` detail. User images serialize as typed
`input_image` parts with a string `image_url` or `file_id`. MCP screenshots and
image tool results remain typed `function_call_output.output` arrays, matching
[OpenCode's Responses serializer](https://github.com/vercel/ai/blob/@ai-sdk/openai@3.0.88/packages/openai/src/responses/convert-to-openai-responses-input.ts).

`Tools::builder().image_generation(true)` enables the shared
`image_gen__imagegen` tool. For Muse sessions it makes a separate `/responses`
request with `model: "muse-image-1.0"`, `store: false`, user `input_text` plus
optional `input_image` references, and only
`tools: [{"type":"image_generation","output_format":"png","size":"auto"}]`.
Completed `image_generation_call.result` bytes return as image content to Spark.
Local files, uploaded references, and recent conversation images can be used for
edits. PNG/JPEG/WebP formats determine the MIME type and artifact extension.
The caller chooses the workspace for saved artifacts. Muse Image always produces
opaque images; transparent requests return an explicit tool error. Subscription
credentials require Responses because the Images endpoints reject them. Account
access and quota still determine whether Muse Image is available.

See [Meta image input](https://dev.meta.ai/docs/image-understanding) and
[Meta image generation](https://dev.meta.ai/docs/image-generation).
