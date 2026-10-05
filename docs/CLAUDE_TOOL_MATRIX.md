# Claude-native tool implementation matrix

This inventory distinguishes the portable library adapters from the native CLI
host that installs them. Tool names alone do not establish Claude Code parity.
The [runtime guide](CLAUDE_RUNTIME.md) describes context, caching, recovery and
host boundaries; the [managed guide](CLAUDE_MANAGED.md) describes the separate
managed product surface.

Claude uses native Messages definitions and results. The CLI must not advertise
Responses `exec_command`, `apply_patch`, `web__run`, `exec`, `wait`, or
`tool_search` as Claude-native tools. A private host implementation may reuse
process, transport or task-registry services without exposing their provider
schemas. The standalone `nanocodex-claude-tools` crate has no OpenAI dependency.

## Pinned capture inventory

The OrcaPromptVault comparison is pinned to commit
`33ce5a020cfcb5fe747d40d0a89e84743fabdd40`. Its September terminal captures
contain conditional product catalogs, not one universal tool protocol:

| Capture | Names in that capture | Differences from the 35-name interactive set |
| --- | ---: | --- |
| Opus 5 interactive | 35 | None |
| Fable 5.1 interactive | 35 | None |
| Fable 5.1 print / Agent SDK | 29 | Omits `Artifact`, `AskUserQuestion`, `EndConversation`, `EnterPlanMode`, `ExitPlanMode`, `SendFeedback` |
| Opus 4.8 interactive | 33 | Omits `EndConversation`, `SendFeedback` |

The following groups account for every name in that 35-name union. Availability
and behavior in Nanocodex are detailed below; sharing a name does not imply
identical options, permissions or lifecycle semantics.

| Captured group | Complete names |
| --- | --- |
| Files and shell | `Bash`, `Edit`, `Glob`, `Grep`, `NotebookEdit`, `Read`, `Write` |
| Agent lifecycle | `Agent`, `ListAgents`, `SendMessage`, `TaskOutput`, `TaskStop` |
| Interaction | `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode` |
| Context and web | `Skill`, `WebFetch`, `WebSearch` |
| Workspace and scheduling | `EnterWorktree`, `ExitWorktree`, `CronCreate`, `CronDelete`, `CronList`, `ScheduleWakeup`, `Monitor` |
| Conditional capabilities absent from this native host | `Artifact`, `DesignSync`, `EndConversation`, `PowerShell`, `PushNotification`, `RemoteTrigger`, `ReportFindings`, `SendFeedback`, `ShareOnboardingGuide`, `Workflow` |

Native `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TodoWrite`,
`ToolSearch`, MCP resource/discovery operations, and Nanocodex extensions
`ProjectContext`, `CloseAgent`, and `SubmitResult` are additional capabilities;
they do not increase coverage of those 35 captured names. `LSP` and
`SubagentHandback` are also absent here and do not occur in these pinned catalogs.
Server-side Anthropic tools use a separate versioned API. Neither catalog size
nor the capture model names establish current provider availability or launch
status.

## Capability boundaries

| Capability | Implementation and limits |
| --- | --- |
| Text files | `ClaudeWorkspaceFiles` implements `Read`, `Write`, `Edit`, `Glob`, and `Grep`. Text files and results are bounded; ambiguous edits fail without mutation. Paths are restricted to the configured workspace, with symlink checks. These checks are not OS confinement of other tools or protection against every concurrent filesystem race. |
| Search | Glob matching supports wildcards, character classes and alternation through `globset`; Grep has content/files/count modes, context, pagination, case control and a bounded explicit file-type map. Rust regex semantics and traversal/output limits differ from ripgrep/Claude Code. Unsupported types/options fail explicitly. |
| Prompt images | Ordered text and HTTPS/data/local image inputs become native Messages blocks. Local bytes are frozen before execution and retained through durable replay; opaque provider file IDs and audio fail explicitly. WASM supports URLs/data URLs, not local files. See the runtime guide for bounds and recovery behavior. |
| Media and notebooks | `execute_output` preserves Claude image blocks for supported images, PDF page rasters and notebook image outputs. PDF reading needs host-installed `pdfinfo` and `pdftoppm`; missing helpers, encrypted PDFs, invalid ranges and excessive data fail explicitly. The CLI also installs the bounded `NotebookEdit` adapter. The text-only compatibility API cannot represent media. |
| Bash | The native CLI installs a retained local shell host, including `run_in_background`, `TaskOutput`, and `TaskStop`. Jobs have bounded capture, deadlines and descendant cleanup. They are process-local, not restored from SQLite. Foreground commands retain their observed final directory inside the configured workspace, including nonzero exits. Background commands snapshot that directory without changing it. Out-of-workspace or unavailable final directories reset subsequent commands to the workspace root. Environment changes and cwd do not survive process restart. No PTY, foreground auto-backgrounding, or Claude Code permission-mode equivalence is promised. The portable `ClaudeBash` adapter still requires an injected executor. |
| Task board | `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, and `TodoWrite` use a bounded session board. The CLI attaches the board. With shared durability its content and next-ID watermark reopen with committed receipts. This board is distinct from retained Bash jobs and the child-agent registry. |
| Agents and messaging | With subagents enabled, the CLI supplies `Agent`, `ListAgents`, `SendMessage`, `CloseAgent`, and child-only `SubmitResult`, plus agent handling in `TaskOutput`/`TaskStop`, through the existing authorized task-tree registry. General-purpose agents start clean conversations with cross-family model routing. Native `subagent_type: fork` starts a background child on the same model from the completed conversation boundary before the current tool batch, with a new session and independent effect receipts. Custom `.claude/agents` definitions, named teams and `isolation: worktree` are not supplied. `CloseAgent` and `SubmitResult` are Nanocodex extensions, not a claim to implement Claude Code `SubagentHandback`. |
| Permission rules | Explicit `--claude-permissions` JSON and `--permission-mode` implement deny > ask > allow, conservative Bash/path matching, exact interactive call approval, final post-hook input rechecks, and persisted rules. Modes include manual/default, acceptEdits, plan, dontAsk, and full-access/bypassPermissions; no auto classifier. Unconfigured sessions retain full-access compatibility. This is dispatch admission, not OS isolation or the complete Claude Code permission system. |
| Questions and plan mode | Interactive CLI/TUI sessions install `AskUserQuestion`, `EnterPlanMode`, and `ExitPlanMode` with pending user input and a dispatch guard. Only explicit approval leaves planning. The guard blocks new model workspace mutations, shell/MCP/agents and unknown capabilities before hooks, and persists plan state. Inspection, context/skill loading and task-board updates remain available through configured hooks. Trusted hooks and previously admitted work can have effects; this is not OS isolation or the full Claude Code permission system. Headless sessions omit the interaction tools but retain the guard on restored planning state. |
| Worktrees | Native `EnterWorktree` creates an owned branch/worktree under `.claude/worktrees` from the exact repository root and persists session workspace transitions. File, shell, context/skills, hooks/checkpoints and new children resolve that workspace; existing background jobs and children remain pinned. `ExitWorktree` defaults to KEEP. Explicit cleanup rejects dirty/untracked/ignored files, new commits, identity changes and pinned contexts. Existing/external paths are not adopted; uncertain Git effects require inspection. |
| Skills and context | The CLI installs `Skill` and the Nanocodex `ProjectContext` extension. `ClaudeSkills` expands arguments and enforces model/user invocation metadata at the host boundary. `ClaudeProjectContext` loads bounded project instructions, local imports and scoped rules. No ambient home/parent discovery, plugin installation, dynamic shell interpolation, forked skill execution, or permissions granted by skill frontmatter. See the runtime guide for CLI integration and remaining differences. |
| MCP and discovery | `ClaudeMcp` retains exact native schemas, error state, structured data, metadata and supported ordered media through a caller-owned `ClaudeMcpProvider`. The host transport has native discovery/call/resource/wait operations, preserving configured exposure and credentials. `ClaudeTools::dynamic_tools` refreshes the catalog before model requests. The CLI attaches `ToolSearch`, `WaitForMcpServers`, `ListMcpResourcesTool`, `ReadMcpResourceTool`, and configured native MCP tools. Search refreshes remote discovery and returns native deferred `tool_reference` blocks plus schema data. Only successful discovery receipts admit deferred calls; execution rechecks current availability and exact admitted schema before hooks or remote effects. Changes are observed at discovery boundaries, not through continuous remote notifications. Unsupported media fails explicitly. |
| Web | `ClaudeWeb` requires a host-approved provider. Native CLI `--web-search` installs client `WebSearch` through an auxiliary Messages server-search request, and `WebFetch` through bounded public HTTPS capture plus auxiliary summarization. Fetch rejects credentials, proxies, private/reserved addresses, unsupported content and excessive redirects; each redirect is revalidated. This is an explicit host policy, not the full Claude Code domain-approval UX. Both paths may incur auxiliary inference charges. |
| Hooks | The tool runtime supports pre-tool decisions and post-success/failure callbacks with committed receipt recovery. Native `--claude-hooks PATH` selects synchronous command hooks explicitly. Supported events are `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`; other events/kinds fail validation. It does not load arbitrary repository settings automatically, implement the full hooks lifecycle, or provide a hook approval UI. |
| Scheduling | Interactive TUI sessions install `CronCreate`, `CronList`, `CronDelete`, and `ScheduleWakeup` unless disabled with `CLAUDE_CODE_DISABLE_CRON=1`. Session-local, persisted cron runs only while open and idle; no daemon, jitter or exactly-once delivery. Reopen skips missed recurring intervals and drops elapsed one-shots/dynamic wakeups. Numeric five-field cron and local/IANA zones are supported; 50-task and seven-day recurring bounds apply. Children/headless sessions omit the catalog. |
| Monitor | Interactive owner TUI sessions with scheduling enabled install command-only `Monitor`. Bounded stdout lines enter the serialized idle prompt queue as untrusted data; `TaskOutput` and `TaskStop` inspect or cancel the session-owned process group. Processes pin their workspace, end at CLI teardown and are never restored. WebSocket sources and reference-style event batching are unsupported; output or queue overflow stops the job. |
| Conditional tools | `PowerShell`, `LSP`, `Workflow`, `SubagentHandback`, and `EndConversation` are not supplied by these native adapters. `Artifact`, `DesignSync`, `PushNotification`, `RemoteTrigger`, `ReportFindings`, `SendFeedback`, and `ShareOnboardingGuide` require separate product capabilities. Their availability in a reference capture is not universal. |
| Platform tools | Versioned Anthropic server search/fetch/tool-search/code-execution definitions and native result replay are separate from Claude Code tools. They require explicit opt-in and provider support; synthetic protocol tests do not establish live admission or billing. |
| User functions | Caller-provided native `ToolDefinition` handlers preserve invocation identity, ordered error results, structured event data and cancellation/replay boundaries. No Codex definitions are implicitly installed. |

## Remaining application differences

Remaining native host differences include complete Claude Code permission-mode
equivalence and the conditional product capabilities above. Native `resume --claude [SESSION_ID]` discovers
and reopens default SQLite journals with saved model/workspace metadata;
explicit custom local-durability stores use their own state-ID recovery path. Durability restores conversation and
receipts. Native `rewind SESSION_ID --checkpoint TURN_ID --restore`
restores before-images from `Edit`, `Write`, and `NotebookEdit`, with explicit
preview and conflict checks. It does not rewind conversation, Bash, hooks or MCP
effects; interrupted multi-file restoration requires inspection. PTYs and
workflow orchestration remain separate work. No full Claude Code parity is claimed.

The local shell is an authorized host process, not an OS sandbox merely because
its executor trait contains the word `Sandbox`. The planning guard must remain installed at dispatch across direct tools and
dynamic capabilities; prompt instructions or a successful `EnterPlanMode`
receipt alone cannot enforce the restriction.

## Evidence and references

Repository acceptance uses the actual CLI or public library with loopback
Messages/SSE and MCP transports, synthetic authentication and real local effects.
See `bin/nanocodex/tests/claude_host.rs`,
`scripts/tests/claude-native-cli-journey.py`, and the runtime guide's acceptance
commands. Inspect command logs and transcripts under ignored `output/`; an
adapter schema, compile check, or passing test name alone is not E2E evidence.
Live subscription observations, managed admission and synthetic host journeys
are separate acceptance boundaries.

The primary compatibility references are the current
[tools reference](https://code.claude.com/docs/en/tools-reference),
[permissions](https://code.claude.com/docs/en/permissions),
[CLI reference](https://code.claude.com/docs/en/cli-reference),
[checkpointing](https://code.claude.com/docs/en/checkpointing),
[scheduling](https://code.claude.com/docs/en/scheduled-tasks),
[skills](https://code.claude.com/docs/en/skills), and
[hooks](https://code.claude.com/docs/en/hooks).

The pinned [OrcaPromptVault capture](https://github.com/Continuum-AI-Corp/OrcaPromptVault/tree/33ce5a020cfcb5fe747d40d0a89e84743fabdd40/Claude-Code)
provides a historical comparison of conditional September catalogs: interactive
and print modes expose different tool sets. It is reference data, not runtime
authority or a complete current contract. Nanocodex instructions are independently
authored; captured vendor prompts and product identity are not bundled.
