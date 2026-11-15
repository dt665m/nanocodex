import type {
  BeforeCompactionRequest,
  CodeEffectJournal,
  CodeEvaluator,
  CompactionReceipt,
  DefaultAgent,
  ExecutionEnvironment,
  HarnessFamily,
  HarnessModel,
  McpServers,
  ReasoningMode,
  SessionCheckpoint,
  Thinking,
  ToolConfiguration,
} from "../types.mjs";
import type { ClaudeTransport } from "./claude-transport.mjs";
import type { Tool as ClaudeTool } from "./claude.mjs";
import type { Tool as SubagentTool } from "./subagents.mjs";
import type { Workspace } from "./workspace.mjs";

/** Passed to a tools factory; called once per root Agent and once per alternate-family child capability. */
export type ToolFactoryContext = Readonly<{ harness: HarnessFamily; role: "root" | "child" }>;
/** Declarative tools are copied per Agent. A factory may return (or resolve) a fresh createTools() instance each call; the Agent host that claims it closes it. */
export type ToolSource<Tools> = Tools | ((context: ToolFactoryContext) => Tools | Promise<Tools>);

type BackendBase = {
  /** The model's working directory label. Grants no file access by itself. */
  workspace?: string | undefined;
  /** Rooted file capability; fixes workspace to its root. Shared adapters share files, never execution state. */
  filesystem?: Workspace | undefined;
  /** true (default): only exec/wait reach the model. false: host tools are exposed directly. */
  codeMode?: boolean | undefined;
};

export type CodexBackend<Transport> = Readonly<BackendBase & {
  transport: Transport;
  tools?: ToolSource<ToolConfiguration<SubagentTool>> | undefined;
  reasoningMode?: ReasoningMode | undefined;
  fastMode?: boolean | undefined;
  instantToolSteering?: boolean | undefined;
  rawApiEvents?: boolean | undefined;
  additionalInstructions?: string | undefined;
  mcp?: McpServers | false | undefined;
  executionEnvironment?: ExecutionEnvironment | undefined;
  filesystemTools?: boolean | undefined;
  beforeCompaction?: ((request: BeforeCompactionRequest) => Promise<CompactionReceipt>) | undefined;
  codeEffectJournal?: CodeEffectJournal | undefined;
}>;

export type ClaudeBackend = Readonly<BackendBase & {
  transport: ClaudeTransport;
  /** Claude tool arrays only; createTools() runtimes are a Codex tool contract. */
  tools?: ToolSource<readonly ClaudeTool[]> | undefined;
  maxTokens?: number | undefined;
  cache?: "off" | "5m" | "1h" | undefined;
  adaptiveThinking?: boolean | undefined;
  keepThinking?: boolean | undefined;
  fastMode?: boolean | undefined;
  parallelTools?: boolean | undefined;
  contextWindowTokens?: number | undefined;
  autoCompactWindowTokens?: number | undefined;
  subagents?: Readonly<{ maxConcurrency?: number | undefined }> | undefined;
}>;

export type Options<Transport> = Readonly<{
  /** Explicit credentials per family; a missing family is never substituted. */
  backends: Readonly<{
    codex?: Transport | CodexBackend<Transport> | undefined;
    claude?: ClaudeTransport | ClaudeBackend | undefined;
  }>;
  /** Default family: else the explicit model's family, else codex when configured, else the only backend. */
  harness?: HarnessFamily | undefined;
  /** Default model, validated against the bundled catalog and harness; defaults to the family's catalog default. */
  model?: HarnessModel | (string & {}) | undefined;
  /** Defaults to the model's own default thinking. */
  thinking?: Thinking | undefined;
  instructions?: string | undefined;
  workspace?: string | undefined;
  codeEvaluator?: CodeEvaluator | undefined;
  /** Compiled WASM module for Web API placements. */
  module?: unknown;
}>;

export type SpawnOptions = Readonly<{
  instructions?: string | undefined;
  /** Same family keeps the default model; another family uses its default model. */
  harness?: HarnessFamily | undefined;
  /** Selects its own family; changing the model uses that model's default thinking (the default model keeps the runtime thinking). */
  model?: HarnessModel | (string & {}) | undefined;
  thinking?: Thinking | undefined;
  workspace?: string | undefined;
}>;

export type ResumeOptions = Readonly<{
  /** Required only for an uncataloged checkpoint model; must match a cataloged one. */
  harness?: HarnessFamily | undefined;
  instructions?: string | undefined;
  workspace?: string | undefined;
}>;

export type Defaults = Readonly<{ harness: HarnessFamily; model: string; thinking: Thinking | undefined }>;

export type Runtime = Readonly<{
  placement: "node" | "host" | "browser";
  defaults: Defaults;
  backends: readonly HarnessFamily[];
  /** A new independent root Agent; forks and session.spawn siblings are also runtime-owned. */
  spawn(options?: SpawnOptions): Promise<DefaultAgent>;
  /** Reopens a checkpoint with the runtime's current credentials and tools. */
  resume(checkpoint: SessionCheckpoint, options?: ResumeOptions): Promise<DefaultAgent>;
  /** Waits for admissions, then cancels and joins every owned Agent, including disposed handles. */
  shutdown(): Promise<void>;
}>;
