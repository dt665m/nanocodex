import type { ResponsesTransport } from "../browser/Transport.mjs";
import type * as Runtime from "../runtime/nanocodex.mjs";

export type Options = Runtime.Options<ResponsesTransport>;
export type CodexBackend = Runtime.CodexBackend<ResponsesTransport>;
export type {
  ClaudeBackend,
  Defaults,
  ResumeOptions,
  Runtime,
  SpawnOptions,
  ToolFactoryContext,
  ToolSource,
} from "../runtime/nanocodex.mjs";

/** Configures backends once and returns a runtime that spawns independent root Agents. */
export function create(options: Options): Promise<Runtime.Runtime>;
