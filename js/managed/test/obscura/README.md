Run from the repository root after the normal workspace install:

```sh
corepack pnpm --filter nanocodex-managed-service test:obscura
```

The runner uses the managed package's esbuild and explicitly pinned workerd
(`1.20260929.1`, new enough for the production bundle's compatibility date).
Set `WORKERD=/absolute/path/to/workerd` to exercise a different runtime. No global
workerd or external network service is needed. The HTTP listener uses an ephemeral
loopback port; all browser traffic goes to a local fixture Worker, including
unexpected URLs (404). Runtime startup and the journey have bounded timeouts.

The 41 observable checks exercise the real `agents` browser helper API through
`createObscuraBrowserBinding`, WorkerLoader, and the current packaged assets in
`src/obscura-assets`. They cover discovery and caching, session creation/deletion,
WebSocket CDP transport, scripts, child-frame contexts, DOM traversal and typing,
promises, errors, unsupported screenshots, stale contexts, cookies and storage
isolation across navigations/tabs/origins, and cleanup. This is a helper API
integration journey; it does not exercise BrowserConnector, the full managed
Worker, a rendered browser, or external sites.

Each run retains `result.json` (checks, actual values, HTTP requests, CDP event
names), `workerd.log`, `control.jsonl`, and `manifest.json` (command, expected and
observed result, package versions, asset hashes) in ignored `output/obscura/run-*`.
The generated Worker bundles, config, and asset snapshot are retained there too.
A failing run exits nonzero and preserves its error and partial journey evidence.

The pinned `agents@0.22.0` package does not publish `src/browser` files. Its source
maps include those exact TypeScript sources. The runner prefers installed source
files when available, otherwise extracts `sourcesContent` from the installed
package into the run's `sdk` directory. `sdk-sources.json` pins their SHA-256 hashes;
the runner checks the package version and every helper before bundling. No helper
implementation is vendored or fetched. The installed package's Apache-2.0 LICENSE
and the precise source provenance are copied alongside the generated helper files.
When upgrading agents, review its browser helper API and deliberately update the
version and hashes here.

The test does not rebuild production assets: run the existing `prepare:obscura`
script after changing the engine, then rerun this command. This makes the journey
validate the same packaged bytes used by the managed Worker.
