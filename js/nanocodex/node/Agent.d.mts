import type {
  AgentLifecycle,
  AgentOptions,
  CodeEvaluator,
  CodeEffectJournal,
  DefaultAgent,
  DurabilityStore,
  McpServers,
  ToolConfiguration,
} from "../types.mjs";
import type { ManagedTransport, ResponsesTransport } from "./Transport.mjs";
import type { Tool as SubagentTool } from "../runtime/subagents.mjs";
import type { Workspace } from "./workspace.mjs";
import type { Tools } from "../tools/Tools.mjs";

export type Agent = DefaultAgent;
/** Code Mode (exec/wait) by default; `direct` exposes host tools to the provider without code evaluation. Remote MCP requires Code Mode. */
type ToolExposureOptions =
  | { mcp?: false | undefined; toolMode?: "code-only" | "direct" | undefined }
  | { mcp: McpServers; toolMode?: "code-only" | undefined };

/** Creates a Node-hosted Rust/WASM Agent. */
export function create(options: create.ManagedOptions): Promise<AgentLifecycle>;
export function create(options: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type ManagedOptions = Readonly<{
    transport: ManagedTransport;
    tools?: Tools | undefined;
  }>;
  /** Codex (OpenAI Responses) harness options. */
  type CodexOptions = AgentOptions & ToolExposureOptions & {
    codeEvaluator?: CodeEvaluator | undefined;
    /** Opt-in durable application-tool and Code Mode receipts for safe cold recovery. */
    codeEffectJournal?: CodeEffectJournal | undefined;
    /** Caller-owned rooted filesystem mounted through standard workspace tools. */
    filesystem?: Workspace | undefined;
    module?: unknown;
    transport: ResponsesTransport;
  } & (
    | {
      durability?: undefined;
      durabilityId?: undefined;
      tools?: ToolConfiguration<SubagentTool> | undefined;
    }
    | {
      durability: DurabilityStore;
      durabilityId: string;
      /** The root remains durable, and so is every fork, side conversation and subagent: each reports its own `session.persistence()`. */
      tools?: ToolConfiguration<SubagentTool> | undefined;
    }
  );
  /** Claude (Messages) harness options; see docs/CLAUDE_JAVASCRIPT.md. */
  type ClaudeOptions = import('../runtime/claude.mjs').Options & { harness: 'claude' };
  /** Options of either harness family, discriminated by `harness`; both return the same Agent. */
  type Options = CodexOptions | ClaudeOptions;
  type ReturnType = Agent;
}
