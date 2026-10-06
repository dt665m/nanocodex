# Reference implementation

The agent, lifecycle, execution, context, rollout, snapshot, error, and usage
sources in `src/` were copied from `nanocodex-agent` at upstream commit
`8bfb67ad2dbb7d3b5d031cc44d563fd2471355e3`. The shared crate is unchanged.
The native image decoder/cache and image tool were lifted from
`nanocodex-oai-tools` at the same revision. Workspace licensing and attribution
apply to these copies.

Muse-specific differences are concentrated in:

- `model/run/{lifecycle,state,turn}.rs`: Claude summary reserve, compaction
  suppression/throttling, summary installation and retained tool/reasoning suffix.
- `model/run/{mod,tool_calls,responses}.rs`: local image preparation, direct image
  tool history, and the Muse Image handler installed into the imported ToolRuntime.
- `image/` and `image_generation/`: Responses URL/low-detail inputs and Muse Image's
  Responses payload, preserving upstream decoding, caches and artifact handling.
- `muse.rs`, `muse_model.rs`, `dialect.rs`, `auth.rs`: provider selection, metadata,
  wire translation, OAuth and exchange. These are owned by this provider.

The copy also adjusts rustdoc crate paths. New HTTP/SSE journeys exercise the provider; inherited reference
checks remain in the copy.

Shared Responses, event, image content, tool and auth-source contracts remain
imports. `nanocodex-oai-api` contains only generic provider metadata registration
and wire adapter hooks, with no Muse identifiers, rates, prompts or policies.
`nanocodex-oai-tools` implementation remains unchanged; existing generic test-fixture repairs are retained.

The original loop's private control/snapshot interfaces require keeping its
supporting lifecycle modules with this copy. The Muse builder therefore returns
Muse-owned lifecycle and snapshot types, exposed through `nanocodex::muse`.
