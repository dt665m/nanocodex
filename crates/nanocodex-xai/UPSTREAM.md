# Grok Build source adaptation

This crate adapts the portable Responses conversation and stream policies from
[SpaceXAI Grok Build](https://github.com/xai-org/grok-build), licensed under Apache
License 2.0. The upstream copyright and complete license are retained in
[THIRD-PARTY-LICENSES](THIRD-PARTY-LICENSES).

- Public repository commit: `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`.
- Upstream monorepo `SOURCE_REV`: `559751fdcec02d413e4c57c8832ab275e4f44980`.
- Copyright 2023–2026 SpaceXAI.

The following files contain modified adaptations; this paragraph and their
file headers give notice of changes under Apache License 2.0 section 4(b):

| Local file | Upstream source | Adaptation |
| --- | --- | --- |
| `src/conversation.rs` | `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` and `sanitize_tool_arguments` in its parent `conversation.rs` | Replace typed `ConversationItem`/`async-openai` conversion with native JSON values; preserve emitted item order directly; retain reasoning status removal, required reasoning text discriminator, function call/result identity, invalid-argument sanitization on replay, and hosted-tool separation. |
| `src/stream.rs` | `crates/codegen/xai-grok-sampler/src/stream/responses.rs` | Adapt terminal-state policy to JSON events; use a bounded incremental SSE decoder instead of the upstream transport/type stack; reject incomplete output without upstream salvage/recovery. |
| Shared xAI model catalog | `crates/codegen/xai-grok-models/default_models.json` | Expose the pinned Grok 4.6/4.5 IDs and supported effort choices through Nanocodex family selectors. |

`src/lib.rs` connects those policies to Nanocodex's public lifecycle, explicit
caller-owned HTTP credentials, and application-supplied function callbacks.
It samples full native Responses history and appends paired function outputs
before the next sample. It does not convert requests through Claude Messages.

This is a bounded in-process source adaptation, not an embedded copy of the
entire Grok CLI. It does not invoke an installed Grok executable, implement ACP,
read Grok subscription credentials, or ship the upstream terminal/filesystem
built-ins, permission engine, plugins, MCP runtime, persistence, compaction,
doom-loop recovery, or automatic retry policies. Unsupported shared lifecycle
operations return explicit errors. Host tools remain the embedding application's
responsibility. Tests replace only the external model provider with local HTTP
and SSE fixtures; they do not establish live xAI account access or model quality.
